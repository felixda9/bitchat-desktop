#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod location;

use bitchat_core::runtime::{App, Command};
use bitchat_core::storage::Storage;
use std::sync::Arc;
use tauri::{Emitter, Manager, State};

fn send(app: &State<'_, App>, command: Command) {
    app.send(command);
}

#[tauri::command]
fn join(app: State<'_, App>, geohash: String, manual: bool) {
    send(&app, Command::Join { geohash, manual });
}

#[tauri::command]
fn leave(app: State<'_, App>) {
    send(&app, Command::Leave);
}

#[tauri::command]
fn submit(app: State<'_, App>, text: String) {
    send(&app, Command::Submit { text });
}

#[tauri::command]
fn open_dm(app: State<'_, App>, peer: Option<String>) {
    send(&app, Command::OpenDm { peer });
}

#[tauri::command]
fn set_nickname(app: State<'_, App>, nickname: String) {
    send(&app, Command::SetNickname { nickname });
}

#[tauri::command]
fn set_tor(app: State<'_, App>, enabled: bool) {
    send(&app, Command::SetTor { enabled });
}

#[tauri::command]
fn add_relay(app: State<'_, App>, url: String) {
    send(&app, Command::AddRelay { url });
}

#[tauri::command]
fn remove_relay(app: State<'_, App>, url: String) {
    send(&app, Command::RemoveRelay { url });
}

#[tauri::command]
fn set_location(app: State<'_, App>, lat: Option<f64>, lon: Option<f64>) {
    let fix = lat.zip(lon);
    send(&app, Command::SetLocation { fix });
}

/// Asks the OS for a location fix and hands it to the core.
#[tauri::command]
async fn locate(app: State<'_, App>) -> Result<(f64, f64), String> {
    let fix = location::locate().await?;
    app.send(Command::SetLocation { fix: Some(fix) });
    Ok(fix)
}

#[tauri::command]
fn refresh(app: State<'_, App>) {
    send(&app, Command::Refresh);
}

#[tauri::command]
fn wipe(app: State<'_, App>) {
    send(&app, Command::Wipe);
}

#[tauri::command]
fn favorite(app: State<'_, App>, peer: String, on: bool) {
    send(&app, Command::Favorite { peer, on });
}

#[tauri::command]
fn set_bridge(app: State<'_, App>, enabled: bool) {
    send(&app, Command::SetBridge { enabled });
}

#[tauri::command]
fn send_image(app: State<'_, App>, data: Vec<u8>) {
    send(&app, Command::SendImage { data });
}

/// Raw bytes of an image or voice note, for the UI to show or play.
#[tauri::command]
async fn media(app: State<'_, App>, id: String) -> Result<tauri::ipc::Response, String> {
    match app.media(id).await {
        Some((_, bytes)) => Ok(tauri::ipc::Response::new(bytes)),
        None => Err("gone".into()),
    }
}

#[tauri::command]
fn set_nearby_only(app: State<'_, App>, on: bool) {
    send(&app, Command::SetNearbyOnly { on });
}

/// Shrinks the window to fit above the taskbar (small or scaled screens),
/// centres it there, then shows it.
fn fit_to_screen(window: &tauri::WebviewWindow) {
    if let (Ok(Some(monitor)), Ok(outer), Ok(inner)) = (
        window.current_monitor(),
        window.outer_size(),
        window.inner_size(),
    ) {
        let area = monitor.work_area();
        let margin = (32.0 * monitor.scale_factor()) as u32;
        let width = outer.width.min(area.size.width.saturating_sub(margin));
        let height = outer.height.min(area.size.height.saturating_sub(margin));
        if (width, height) != (outer.width, outer.height) {
            let frame = (outer.width - inner.width, outer.height - inner.height);
            let _ = window.set_size(tauri::PhysicalSize::new(
                width.saturating_sub(frame.0),
                height.saturating_sub(frame.1),
            ));
        }
        let _ = window.set_position(tauri::PhysicalPosition::new(
            area.position.x + (area.size.width.saturating_sub(width) / 2) as i32,
            area.position.y + (area.size.height.saturating_sub(height) / 2) as i32,
        ));
    }
    let _ = window.show();
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();

    tauri::Builder::default()
        .setup(|app| {
            let handle = app.handle().clone();
            let sink = Arc::new(move |event: bitchat_core::model::UiEvent| {
                let _ = handle.emit("bitchat", &event);
            });
            let dir = Storage::default_dir();
            let core = tauri::async_runtime::block_on(async move { App::start(dir, sink) })?;
            app.manage(core);
            for window in app.webview_windows().values() {
                fit_to_screen(window);
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            favorite,
            set_bridge,
            set_nearby_only,
            send_image,
            media,
            join,
            leave,
            submit,
            open_dm,
            set_nickname,
            set_tor,
            add_relay,
            remove_relay,
            set_location,
            locate,
            refresh,
            wipe
        ])
        .run(tauri::generate_context!())
        .expect("failed to start bitchat");
}
