//! Bluetooth LE transport for the bitchat mesh.

use uuid::{Uuid, uuid};

pub mod assembler;
pub mod central;

/// bitchat GATT service (release builds; iOS debug builds use `…4B5A`).
pub const SERVICE_UUID: Uuid = uuid!("F47B5E2D-4A9E-4C5A-9B3F-8E1D2C3A4B5C");
/// The one characteristic: notify + write + write without response + read.
pub const CHARACTERISTIC_UUID: Uuid = uuid!("A1B2C3D4-E5F6-4A5B-8C9D-0E1F2A3B4C5D");
