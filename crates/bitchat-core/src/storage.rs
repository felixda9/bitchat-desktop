//! Encrypted on-disk state.
//!
//! A random 32-byte key lives in the OS keychain (Windows Credential Manager,
//! Linux Secret Service). The state file is `nonce(24) || XChaCha20-Poly1305(json)`.
//! When no keychain is available (headless Linux) the key falls back to a
//! 0600 file next to the state and [`Storage::key_in_keychain`] is false.

use crate::state::State;
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use std::path::{Path, PathBuf};

const KEYRING_SERVICE: &str = "bitchat-desktop";
const KEYRING_USER: &str = "state-key";
const STATE_FILE: &str = "state.bin";
const KEY_FILE: &str = "state.key";

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("keychain: {0}")]
    Keychain(String),
    #[error("state file is corrupt or was written with another key")]
    Corrupt,
}

enum KeySource {
    Keychain,
    File(PathBuf),
    Fixed([u8; 32]),
}

pub struct Storage {
    dir: PathBuf,
    source: KeySource,
}

fn random<const N: usize>() -> [u8; N] {
    let mut out = [0u8; N];
    getrandom::fill(&mut out).expect("OS random number generator failed");
    out
}

impl Storage {
    /// The default per-user data directory.
    pub fn default_dir() -> PathBuf {
        directories::ProjectDirs::from("io.github", "felixda9", "bitchat-desktop")
            .map(|d| d.data_local_dir().to_path_buf())
            .unwrap_or_else(|| std::env::temp_dir().join("bitchat-desktop"))
    }

    pub fn open(dir: PathBuf) -> Result<Storage, StorageError> {
        std::fs::create_dir_all(&dir)?;
        let source = match keyring::Entry::store_status() {
            Ok(()) => KeySource::Keychain,
            Err(e) => {
                log::warn!("no OS keychain ({e}); keeping the state key in a file");
                KeySource::File(dir.join(KEY_FILE))
            }
        };
        Ok(Storage { dir, source })
    }

    /// Storage with a fixed key, for tests.
    pub fn with_fixed_key(dir: PathBuf, key: [u8; 32]) -> Result<Storage, StorageError> {
        std::fs::create_dir_all(&dir)?;
        Ok(Storage {
            dir,
            source: KeySource::Fixed(key),
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn key_in_keychain(&self) -> bool {
        matches!(self.source, KeySource::Keychain)
    }

    /// Arti state and cache directories.
    pub fn tor_dirs(&self) -> (PathBuf, PathBuf) {
        (
            self.dir.join("tor").join("state"),
            self.dir.join("tor").join("cache"),
        )
    }

    fn entry() -> Result<keyring::Entry, StorageError> {
        keyring::Entry::new(KEYRING_SERVICE, KEYRING_USER)
            .map_err(|e| StorageError::Keychain(e.to_string()))
    }

    fn read_key(&self) -> Result<Option<[u8; 32]>, StorageError> {
        let bytes = match &self.source {
            KeySource::Fixed(k) => return Ok(Some(*k)),
            KeySource::Keychain => match Self::entry()?.get_secret() {
                Ok(b) => b,
                Err(keyring::Error::NoEntry) => return Ok(None),
                Err(e) => return Err(StorageError::Keychain(e.to_string())),
            },
            KeySource::File(path) => match std::fs::read(path) {
                Ok(b) => b,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(e) => return Err(e.into()),
            },
        };
        bytes
            .try_into()
            .map(Some)
            .map_err(|_| StorageError::Corrupt)
    }

    fn create_key(&self) -> Result<[u8; 32], StorageError> {
        let key = random::<32>();
        match &self.source {
            KeySource::Fixed(k) => return Ok(*k),
            KeySource::Keychain => Self::entry()?
                .set_secret(&key)
                .map_err(|e| StorageError::Keychain(e.to_string()))?,
            KeySource::File(path) => write_private(path, &key)?,
        }
        Ok(key)
    }

    /// Loads the saved state, or `None` on first run.
    pub fn load(&self) -> Result<Option<State>, StorageError> {
        let path = self.dir.join(STATE_FILE);
        let data = match std::fs::read(&path) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let key = self.read_key()?.ok_or(StorageError::Corrupt)?;
        if data.len() < 24 + 16 {
            return Err(StorageError::Corrupt);
        }
        let (nonce, sealed) = data.split_at(24);
        let nonce = XNonce::try_from(nonce).map_err(|_| StorageError::Corrupt)?;
        let plain = XChaCha20Poly1305::new(&key.into())
            .decrypt(&nonce, sealed)
            .map_err(|_| StorageError::Corrupt)?;
        serde_json::from_slice(&plain)
            .map(Some)
            .map_err(|_| StorageError::Corrupt)
    }

    pub fn save(&self, state: &State) -> Result<(), StorageError> {
        let key = match self.read_key()? {
            Some(k) => k,
            None => self.create_key()?,
        };
        let nonce = random::<24>();
        let json = serde_json::to_vec(state).expect("state serializes");
        let sealed = XChaCha20Poly1305::new(&key.into())
            .encrypt(&XNonce::from(nonce), json.as_slice())
            .map_err(|_| StorageError::Corrupt)?;
        let mut out = Vec::with_capacity(24 + sealed.len());
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&sealed);
        let tmp = self.dir.join(format!("{STATE_FILE}.tmp"));
        write_private(&tmp, &out)?;
        std::fs::rename(tmp, self.dir.join(STATE_FILE))?;
        Ok(())
    }

    /// Deletes the keychain entry and everything in the data directory.
    pub fn wipe(&self) -> Result<(), StorageError> {
        if let KeySource::Keychain = self.source {
            match Self::entry()?.delete_credential() {
                Ok(()) | Err(keyring::Error::NoEntry) => {}
                Err(e) => return Err(StorageError::Keychain(e.to_string())),
            }
        }
        if self.dir.exists() {
            for item in std::fs::read_dir(&self.dir)? {
                let path = item?.path();
                // Tor's directory cache and guard state stay: the running Tor
                // client holds them open, and they hold no bitchat identity.
                if path.file_name().is_some_and(|n| n == "tor") {
                    continue;
                }
                if path.is_dir() {
                    std::fs::remove_dir_all(&path)?;
                } else {
                    std::fs::remove_file(&path)?;
                }
            }
        }
        Ok(())
    }
}

fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        f.write_all(data)
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("bitchat-core-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn round_trip_and_wipe() {
        let dir = temp_dir("storage");
        let storage = Storage::with_fixed_key(dir.clone(), [7; 32]).unwrap();
        assert!(storage.load().unwrap().is_none());
        let state = State::fresh();
        storage.save(&state).unwrap();
        assert_eq!(storage.load().unwrap().unwrap(), state);

        let other = Storage::with_fixed_key(dir.clone(), [8; 32]).unwrap();
        assert!(matches!(other.load(), Err(StorageError::Corrupt)));

        storage.wipe().unwrap();
        assert!(storage.load().unwrap().is_none());
        let _ = std::fs::remove_dir_all(dir);
    }
}
