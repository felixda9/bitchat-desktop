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
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            favorite,
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
