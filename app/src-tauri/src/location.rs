//! One-shot OS location fix: Windows Geolocator, Linux GeoClue2.

#[cfg(windows)]
pub async fn locate() -> Result<(f64, f64), String> {
    tauri::async_runtime::spawn_blocking(|| {
        use windows::Devices::Geolocation::{GeolocationAccessStatus, Geolocator};
        use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize};
        // Already-initialized apartments return an error we can ignore.
        let _ = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };
        let access = Geolocator::RequestAccessAsync()
            .and_then(|op| op.join())
            .map_err(|e| format!("location unavailable: {e}"))?;
        if access != GeolocationAccessStatus::Allowed {
            return Err(
                "location is off — allow desktop apps to use your location in Windows settings"
                    .into(),
            );
        }
        let locator = Geolocator::new().map_err(|e| e.to_string())?;
        let position = locator
            .GetGeopositionAsync()
            .and_then(|op| op.join())
            .and_then(|p| p.Coordinate())
            .and_then(|c| c.Point())
            .and_then(|p| p.Position())
            .map_err(|e| format!("could not get a location fix: {e}"))?;
        Ok((position.Latitude, position.Longitude))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(target_os = "linux")]
pub async fn locate() -> Result<(f64, f64), String> {
    use futures_lite::StreamExt;
    use zbus::{Connection, OwnedObjectPath, Proxy};

    const BUS: &str = "org.freedesktop.GeoClue2";
    fn fail(e: impl std::fmt::Display) -> String {
        format!("location unavailable (geoclue): {e}")
    }

    let conn = Connection::system().await.map_err(fail)?;
    let manager = Proxy::new(
        &conn,
        BUS,
        "/org/freedesktop/GeoClue2/Manager",
        "org.freedesktop.GeoClue2.Manager",
    )
    .await
    .map_err(fail)?;
    let client_path: OwnedObjectPath = manager.call("GetClient", &()).await.map_err(fail)?;
    let client = Proxy::new(&conn, BUS, client_path, "org.freedesktop.GeoClue2.Client")
        .await
        .map_err(fail)?;
    client
        .set_property("DesktopId", "bitchat-desktop")
        .await
        .map_err(fail)?;
    // 8 = exact; GeoClue may still give city-level accuracy.
    client
        .set_property("RequestedAccuracyLevel", 8u32)
        .await
        .map_err(fail)?;

    let mut updates = client
        .receive_signal("LocationUpdated")
        .await
        .map_err(fail)?;
    let _: () = client.call("Start", &()).await.map_err(fail)?;
    let signal = tokio::time::timeout(std::time::Duration::from_secs(30), updates.next())
        .await
        .map_err(|_| "no location fix within 30 seconds".to_string())?
        .ok_or("location updates stopped")?;
    let (_old, new): (OwnedObjectPath, OwnedObjectPath) =
        signal.body().deserialize().map_err(fail)?;
    let location = Proxy::new(&conn, BUS, new, "org.freedesktop.GeoClue2.Location")
        .await
        .map_err(fail)?;
    let lat: f64 = location.get_property("Latitude").await.map_err(fail)?;
    let lon: f64 = location.get_property("Longitude").await.map_err(fail)?;
    let _: Result<(), _> = client.call("Stop", &()).await;
    Ok((lat, lon))
}

#[cfg(not(any(windows, target_os = "linux")))]
pub async fn locate() -> Result<(f64, f64), String> {
    Err("location is not supported on this platform".into())
}
