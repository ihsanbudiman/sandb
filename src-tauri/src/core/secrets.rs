//! Password storage. `VaultStore` keeps every secret in `secrets.enc` in the data dir, encrypted
//! with AES-256-GCM. Only the key of that file is in the OS credential store: Keychain on macOS,
//! Credential Manager on Windows, Secret Service (GNOME Keyring, KWallet) on Linux. Tests and the
//! dev bridge use memory only.
//!
//! macOS ties a Keychain "Always Allow" grant to one build of the app, so each update asks again.
//! With one credential store entry, an update asks once, not once per saved password.
//!
//! Releases up to 0.3.0 kept secrets in `credentials.enc`, encrypted with a key built into the
//! app; `migrate_legacy_file` moves them. Releases 0.4.0 to 0.7.1 kept each secret as its own
//! credential store entry; `migrate_keychain_entries` moves them.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use aes_gcm::aead::{Aead, AeadCore, KeyInit, OsRng};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;

use crate::core::connections::Connection;
use crate::core::error::{AppError, ErrorCode};
use crate::core::store::write_atomic;

/// The pre-keychain password file. Read only by `migrate_legacy_file`.
pub const LEGACY_CREDENTIALS_FILE: &str = "credentials.enc";

/// The secrets of the app, encrypted with the key in `VAULT_KEY_ACCOUNT`.
pub const VAULT_FILE: &str = "secrets.enc";
/// The one credential store entry of the app: the base64 key of `VAULT_FILE`.
pub const VAULT_KEY_ACCOUNT: &str = "vault:key";
/// Secret store account of the AI API key.
pub const AI_KEY_ACCOUNT: &str = "ai:api_key";

/// The key of `credentials.enc`. Kept only to read old files during migration.
const KEY: [u8; 32] = [
    0x75, 0xb9, 0x55, 0x1d, 0x41, 0xdf, 0x71, 0xd9, 0x85, 0xe7, 0xd2, 0x3a, 0xd2, 0x32, 0xbb, 0xfb,
    0x07, 0x66, 0xc4, 0x33, 0xe4, 0x80, 0xc3, 0xf6, 0xdf, 0x85, 0x9b, 0xe0, 0x3f, 0x0a, 0x28, 0xc1,
];
/// File layout of `credentials.enc` and `secrets.enc`, base64-encoded: `VERSION` byte, 12-byte
/// nonce, AES-256-GCM ciphertext of a JSON map from account to secret.
const VERSION: u8 = 1;
const NONCE_LEN: usize = 12;

pub fn password_account(conn_id: &str) -> String {
    format!("conn:{conn_id}:password")
}

/// Every password account of `connections`, in order.
pub fn password_accounts(connections: &[Connection]) -> Vec<String> {
    connections
        .iter()
        .map(|c| password_account(&c.id))
        .collect()
}

pub trait SecretStore: Send + Sync {
    fn get(&self, account: &str) -> Result<Option<String>, AppError>;
    fn set(&self, account: &str, secret: &str) -> Result<(), AppError>;
    /// Deleting a missing entry is not an error.
    fn delete(&self, account: &str) -> Result<(), AppError>;
}

fn secret_err(e: impl std::fmt::Display) -> AppError {
    AppError::new(
        ErrorCode::SecretStore,
        "Could not use the system credential store (Keychain, Credential Manager or Secret Service)",
    )
    .with_detail(e.to_string())
}

/// The credential store entry that holds the base64 key of `VAULT_FILE`, under `service`.
pub struct KeychainStore {
    service: String,
}

impl KeychainStore {
    pub fn new(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
        }
    }

    fn entry(&self, account: &str) -> Result<keyring::Entry, AppError> {
        keyring::Entry::new(&self.service, account).map_err(secret_err)
    }
}

impl SecretStore for KeychainStore {
    fn get(&self, account: &str) -> Result<Option<String>, AppError> {
        match self.entry(account)?.get_password() {
            Ok(s) => Ok(Some(s)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(secret_err(e)),
        }
    }

    fn set(&self, account: &str, secret: &str) -> Result<(), AppError> {
        self.entry(account)?
            .set_password(secret)
            .map_err(secret_err)
    }

    fn delete(&self, account: &str) -> Result<(), AppError> {
        match self.entry(account)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(secret_err(e)),
        }
    }
}

/// Every secret in `VAULT_FILE`; its key in `keychain`.
pub struct VaultStore {
    path: PathBuf,
    keychain: Box<dyn SecretStore>,
    /// Filled at the first use, so the credential store is read once per process.
    vault: Mutex<Option<Vault>>,
}

struct Vault {
    key: [u8; 32],
    map: BTreeMap<String, String>,
}

impl VaultStore {
    pub fn new(data_dir: &Path, keychain: Box<dyn SecretStore>) -> Self {
        Self {
            path: data_dir.join(VAULT_FILE),
            keychain,
            vault: Mutex::new(None),
        }
    }

    fn with<T>(&self, f: impl FnOnce(&mut Vault) -> Result<T, AppError>) -> Result<T, AppError> {
        let mut slot = self.vault.lock().unwrap_or_else(|e| e.into_inner());
        if slot.is_none() {
            *slot = Some(self.load()?);
        }
        f(slot.as_mut().expect("vault loaded above"))
    }

    fn load(&self) -> Result<Vault, AppError> {
        let Some(text) = self.keychain.get(VAULT_KEY_ACCOUNT)? else {
            // A new key would make the file unreadable for good; the user must decide.
            if self.path.exists() {
                return Err(AppError::new(
                    ErrorCode::SecretStore,
                    format!(
                        "The key of {} is not in the credential store. Remove the file and enter your passwords again.",
                        self.path.display()
                    ),
                ));
            }
            let key: [u8; 32] = Aes256Gcm::generate_key(OsRng).into();
            self.keychain.set(VAULT_KEY_ACCOUNT, &B64.encode(key))?;
            return Ok(Vault {
                key,
                map: BTreeMap::new(),
            });
        };
        let key: [u8; 32] = B64
            .decode(text.trim())
            .ok()
            .and_then(|k| k.try_into().ok())
            .ok_or_else(|| {
                AppError::new(
                    ErrorCode::SecretStore,
                    format!(
                        "The key of {} in the credential store is damaged.",
                        self.path.display()
                    ),
                )
            })?;
        let map = read_encrypted(&self.path, &key)?.unwrap_or_default();
        Ok(Vault { key, map })
    }

    /// Writes the file first; memory changes only when the write worked.
    fn change(&self, f: impl FnOnce(&mut BTreeMap<String, String>)) -> Result<(), AppError> {
        self.with(|v| {
            let mut next = v.map.clone();
            f(&mut next);
            if next != v.map {
                write_encrypted(&self.path, &v.key, &next)?;
                v.map = next;
            }
            Ok(())
        })
    }
}

impl SecretStore for VaultStore {
    fn get(&self, account: &str) -> Result<Option<String>, AppError> {
        self.with(|v| Ok(v.map.get(account).cloned()))
    }

    fn set(&self, account: &str, secret: &str) -> Result<(), AppError> {
        self.change(|m| {
            m.insert(account.to_string(), secret.to_string());
        })
    }

    fn delete(&self, account: &str) -> Result<(), AppError> {
        self.change(|m| {
            m.remove(account);
        })
    }
}

/// Moves the `accounts` that `old` has (one credential store entry each, releases 0.4.0 to 0.7.1)
/// into `vault`, then removes them from `old`. A secret that `vault` already has is newer and
/// stays. On an error the remaining entries stay in `old`, and the next start tries again.
pub fn migrate_keychain_entries(
    old: &dyn SecretStore,
    vault: &dyn SecretStore,
    accounts: &[String],
) -> Result<(), AppError> {
    for account in accounts {
        let Some(secret) = old.get(account)? else {
            continue;
        };
        if vault.get(account)?.is_none() {
            vault.set(account, &secret)?;
        }
        old.delete(account)?;
    }
    Ok(())
}

/// Moves the entries of `credentials.enc` in `data_dir` into `store`, then removes the file.
///
/// An entry that `store` already has is not overwritten: after a migration that stopped part way,
/// the user can have changed that secret in the app, and the file copy is older. On any error the
/// file stays, so the next start tries again and no secret is lost.
pub fn migrate_legacy_file(data_dir: &Path, store: &dyn SecretStore) -> Result<(), AppError> {
    let path = data_dir.join(LEGACY_CREDENTIALS_FILE);
    let Some(map) = read_legacy_file(&path)? else {
        return Ok(());
    };
    for (account, secret) in &map {
        if store.get(account)?.is_none() {
            store.set(account, secret)?;
        }
    }
    fs::remove_file(&path).map_err(|e| {
        AppError::new(
            ErrorCode::Io,
            format!("Could not remove {}", path.display()),
        )
        .with_detail(e.to_string())
    })
}

/// `None` when there is no file. A file that does not decrypt is an error, never an empty map:
/// treating it as empty would remove the file and every password in it.
fn read_legacy_file(path: &Path) -> Result<Option<BTreeMap<String, String>>, AppError> {
    read_encrypted(path, &KEY)
}

/// `None` when there is no file. A file that does not decrypt is an error, never an empty map.
fn read_encrypted(
    path: &Path,
    key: &[u8; 32],
) -> Result<Option<BTreeMap<String, String>>, AppError> {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(
                AppError::new(ErrorCode::Io, format!("Could not read {}", path.display()))
                    .with_detail(e.to_string()),
            );
        }
    };
    // No decode or parse detail: it could quote the decrypted content.
    let damaged = || {
        AppError::new(
            ErrorCode::SecretStore,
            format!(
                "{} is damaged. Remove it and enter your connection passwords again.",
                path.display()
            ),
        )
    };
    let raw = B64.decode(text.trim()).map_err(|_| damaged())?;
    if raw.len() < 1 + NONCE_LEN || raw[0] != VERSION {
        return Err(damaged());
    }
    let (nonce, body) = raw[1..].split_at(NONCE_LEN);
    let plain = Aes256Gcm::new(key.into())
        .decrypt(Nonce::from_slice(nonce), body)
        .map_err(|_| damaged())?;
    serde_json::from_slice(&plain)
        .map(Some)
        .map_err(|_| damaged())
}

fn encrypt(key: &[u8; 32], map: &BTreeMap<String, String>) -> String {
    let plain = serde_json::to_vec(map).expect("a string map always serializes");
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let body = Aes256Gcm::new(key.into())
        .encrypt(&nonce, plain.as_slice())
        .expect("AES-GCM encryption of an in-memory buffer does not fail");
    let mut raw = vec![VERSION];
    raw.extend_from_slice(&nonce);
    raw.extend_from_slice(&body);
    B64.encode(raw)
}

fn write_encrypted(
    path: &Path,
    key: &[u8; 32],
    map: &BTreeMap<String, String>,
) -> Result<(), AppError> {
    write_atomic(path, encrypt(key, map).as_bytes(), true).map_err(|e| {
        AppError::new(ErrorCode::Io, format!("Could not write {}", path.display()))
            .with_detail(e.to_string())
    })
}

#[derive(Default)]
pub struct MemoryStore {
    map: Mutex<std::collections::HashMap<String, String>>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl SecretStore for MemoryStore {
    fn get(&self, account: &str) -> Result<Option<String>, AppError> {
        Ok(self.map.lock().unwrap().get(account).cloned())
    }

    fn set(&self, account: &str, secret: &str) -> Result<(), AppError> {
        self.map
            .lock()
            .unwrap()
            .insert(account.to_string(), secret.to_string());
        Ok(())
    }

    fn delete(&self, account: &str) -> Result<(), AppError> {
        self.map.lock().unwrap().remove(account);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_name_format() {
        assert_eq!(password_account("abc"), "conn:abc:password");
    }

    #[test]
    fn memory_store_round_trip() {
        let s = MemoryStore::new();
        assert_eq!(s.get("a").unwrap(), None);
        s.set("a", "pw").unwrap();
        assert_eq!(s.get("a").unwrap().as_deref(), Some("pw"));
        s.delete("a").unwrap();
        s.delete("a").unwrap();
        assert_eq!(s.get("a").unwrap(), None);
    }

    /// Writes `map` the way releases up to 0.3.0 did.
    fn write_legacy(dir: &Path, map: &BTreeMap<&str, &str>) {
        let map = map
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        fs::write(dir.join(LEGACY_CREDENTIALS_FILE), encrypt(&KEY, &map)).unwrap();
    }

    /// Users who update from a release with `credentials.enc` keep every saved password, and the
    /// file with the built-in key is gone afterwards.
    #[test]
    fn migration_moves_every_entry_and_removes_the_file() {
        let dir = tempfile::tempdir().unwrap();
        write_legacy(
            dir.path(),
            &BTreeMap::from([("conn:a:password", "pw-a"), ("ai:api_key", "sk-1")]),
        );
        let s = MemoryStore::new();
        migrate_legacy_file(dir.path(), &s).unwrap();
        assert_eq!(s.get("conn:a:password").unwrap().as_deref(), Some("pw-a"));
        assert_eq!(s.get("ai:api_key").unwrap().as_deref(), Some("sk-1"));
        assert!(!dir.path().join(LEGACY_CREDENTIALS_FILE).exists());
        // Later starts find no file and change nothing.
        migrate_legacy_file(dir.path(), &s).unwrap();
        assert_eq!(s.get("conn:a:password").unwrap().as_deref(), Some("pw-a"));
    }

    /// A secret already in the store is newer than the file copy (an earlier migration stopped
    /// part way and the user changed it since); the old file value must not bring it back.
    #[test]
    fn migration_keeps_entries_the_store_already_has() {
        let dir = tempfile::tempdir().unwrap();
        write_legacy(dir.path(), &BTreeMap::from([("conn:a:password", "old")]));
        let s = MemoryStore::new();
        s.set("conn:a:password", "new").unwrap();
        migrate_legacy_file(dir.path(), &s).unwrap();
        assert_eq!(s.get("conn:a:password").unwrap().as_deref(), Some("new"));
    }

    struct FailingStore;
    impl SecretStore for FailingStore {
        fn get(&self, _: &str) -> Result<Option<String>, AppError> {
            Ok(None)
        }
        fn set(&self, _: &str, _: &str) -> Result<(), AppError> {
            Err(secret_err("locked"))
        }
        fn delete(&self, _: &str) -> Result<(), AppError> {
            Ok(())
        }
    }

    /// When the credential store refuses a write (locked, no Secret Service), the file is the only
    /// copy of the passwords: it must stay so the next start can try again.
    #[test]
    fn failed_migration_keeps_the_file() {
        let dir = tempfile::tempdir().unwrap();
        write_legacy(dir.path(), &BTreeMap::from([("conn:a:password", "pw")]));
        let e = migrate_legacy_file(dir.path(), &FailingStore).unwrap_err();
        assert_eq!(e.code, ErrorCode::SecretStore);
        assert!(dir.path().join(LEGACY_CREDENTIALS_FILE).exists());
    }

    /// A damaged file must fail loudly and stay untouched; reading it as empty would remove it
    /// together with every password in it.
    #[test]
    fn damaged_legacy_file_is_an_error_and_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(LEGACY_CREDENTIALS_FILE);
        fs::write(&path, "AQIDBAUGBwgJCgsMDQ4PEBESExQVFhcYGRo=").unwrap();
        let err = migrate_legacy_file(dir.path(), &MemoryStore::new()).unwrap_err();
        assert_eq!(err.code, ErrorCode::SecretStore);
        // The user can only recover by removing the file, so the error must name it.
        assert!(err.message.contains(&path.display().to_string()));
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "AQIDBAUGBwgJCgsMDQ4PEBESExQVFhcYGRo="
        );
    }

    /// A credential store that outlives one `VaultStore` (like the real Keychain across app
    /// starts) and counts its reads.
    #[derive(Clone, Default)]
    struct SharedKeychain {
        inner: std::sync::Arc<MemoryStore>,
        gets: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }
    impl SecretStore for SharedKeychain {
        fn get(&self, account: &str) -> Result<Option<String>, AppError> {
            self.gets.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.inner.get(account)
        }
        fn set(&self, account: &str, secret: &str) -> Result<(), AppError> {
            self.inner.set(account, secret)
        }
        fn delete(&self, account: &str) -> Result<(), AppError> {
            self.inner.delete(account)
        }
    }

    /// Each credential store read can mean a macOS prompt after an update, so one process must
    /// read it once, however many secrets it uses. Secrets must survive a restart and must not be
    /// readable in the file.
    #[test]
    fn vault_reads_the_credential_store_once_and_keeps_secrets_across_starts() {
        let dir = tempfile::tempdir().unwrap();
        let keychain = SharedKeychain::default();
        let v = VaultStore::new(dir.path(), Box::new(keychain.clone()));
        v.set("conn:a:password", "pw-a-7f1c").unwrap();
        v.set("conn:b:password", "pw-b-7f1c").unwrap();
        v.set("ai:api_key", "sk-7f1c").unwrap();
        v.delete("conn:b:password").unwrap();
        assert_eq!(
            v.get("conn:a:password").unwrap().as_deref(),
            Some("pw-a-7f1c")
        );
        assert_eq!(keychain.gets.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(
            keychain.inner.map.lock().unwrap().len(),
            1,
            "only the key is in the store"
        );

        let file = fs::read_to_string(dir.path().join(VAULT_FILE)).unwrap();
        assert!(!file.contains("7f1c"));

        let restarted = VaultStore::new(dir.path(), Box::new(keychain.clone()));
        assert_eq!(
            restarted.get("conn:a:password").unwrap().as_deref(),
            Some("pw-a-7f1c")
        );
        assert_eq!(
            restarted.get("ai:api_key").unwrap().as_deref(),
            Some("sk-7f1c")
        );
        assert_eq!(restarted.get("conn:b:password").unwrap(), None);
    }

    /// Without its key the file can never be read again. A new key would hide that and let the
    /// next save replace every password, so the vault must fail and keep the file.
    #[test]
    fn vault_file_without_its_key_is_an_error_and_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        VaultStore::new(dir.path(), Box::new(SharedKeychain::default()))
            .set("conn:a:password", "pw")
            .unwrap();
        let path = dir.path().join(VAULT_FILE);
        let before = fs::read_to_string(&path).unwrap();

        let v = VaultStore::new(dir.path(), Box::new(SharedKeychain::default()));
        let err = v.set("conn:b:password", "pw").unwrap_err();
        assert_eq!(err.code, ErrorCode::SecretStore);
        assert!(err.message.contains(&path.display().to_string()));
        assert_eq!(fs::read_to_string(&path).unwrap(), before);
    }

    /// Users who update from 0.4.0 to 0.7.1 keep every saved password, and the old entries are
    /// gone, so later updates do not ask for them. A vault value is newer than the old entry.
    #[test]
    fn keychain_migration_moves_entries_and_keeps_newer_vault_values() {
        let old = MemoryStore::new();
        old.set("conn:a:password", "pw-a").unwrap();
        old.set("conn:b:password", "old-b").unwrap();
        old.set("other:entry", "x").unwrap();
        let vault = MemoryStore::new();
        vault.set("conn:b:password", "new-b").unwrap();
        let accounts =
            ["conn:a:password", "conn:b:password", "conn:gone:password"].map(String::from);

        migrate_keychain_entries(&old, &vault, &accounts).unwrap();

        assert_eq!(
            vault.get("conn:a:password").unwrap().as_deref(),
            Some("pw-a")
        );
        assert_eq!(
            vault.get("conn:b:password").unwrap().as_deref(),
            Some("new-b")
        );
        assert_eq!(old.get("conn:a:password").unwrap(), None);
        assert_eq!(old.get("conn:b:password").unwrap(), None);
        assert_eq!(
            old.get("other:entry").unwrap().as_deref(),
            Some("x"),
            "only listed accounts move"
        );
    }
}
