//! Bluetooth mesh protocol: identity, signed packets, announces, fragments.
//! Radio-independent; the BLE transport comes on top.

pub mod announce;
pub mod fragment;
pub mod identity;

pub use identity::{MeshIdentity, verify};
