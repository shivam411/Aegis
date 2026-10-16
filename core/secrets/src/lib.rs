//! Per-app secret environment variables, encrypted at rest.
//!
//! Values are sealed with ChaCha20-Poly1305 under a 256-bit key kept in a
//! file outside the database (`<data_dir>/secrets.key`, mode 0600), so a copy
//! of `aegis.db` alone reveals only secret names. Each value is bound to its
//! project and name (as associated data), so sealed values can't be swapped
//! between slots. Decrypted values only exist in memory, when a process is
//! started.

use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, CHACHA20_POLY1305, NONCE_LEN};
use ring::rand::{SecureRandom, SystemRandom};
use serde::Serialize;
use sqlx::{Row, SqlitePool};
use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::Path;
use std::sync::{Arc, Mutex};

const KEY_LEN: usize = 32;
/// Upper bound for one value (environment blocks are limited by the OS).
pub const MAX_VALUE_LEN: usize = 32 * 1024;
const MAX_NAME_LEN: usize = 128;
/// Names Aegis sets itself.
const RESERVED: &[&str] = &["PORT"];

/// A secret's metadata. The value is never returned by the store's queries.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SecretInfo {
    pub name: String,
    pub updated_at: String,
}

#[derive(Clone)]
struct Sealed {
    bytes: Vec<u8>,
    updated_at: String,
}

struct Inner {
    pool: SqlitePool,
    key: LessSafeKey,
    rng: SystemRandom,
    /// project id -> name -> sealed value; mirrors the `secrets` table.
    cache: Mutex<HashMap<String, BTreeMap<String, Sealed>>>,
}

#[derive(Clone)]
pub struct SecretStore {
    inner: Arc<Inner>,
}

/// Checks that `name` can be used as an environment variable name.
pub fn validate_name(name: &str) -> Result<(), String> {
    let mut chars = name.chars();
    let valid = matches!(chars.next(), Some(c) if c == '_' || c.is_ascii_alphabetic())
        && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
        && name.len() <= MAX_NAME_LEN;
    if !valid {
        return Err(format!(
            "'{}' is not a valid variable name: use letters, digits and _, not starting with a digit (max {} characters)",
            name, MAX_NAME_LEN
        ));
    }
    if RESERVED.contains(&name) {
        return Err(format!("{} is set by Aegis and can't be overridden", name));
    }
    Ok(())
}

pub fn validate_value(value: &str) -> Result<(), String> {
    if value.contains('\0') {
        return Err("Values can't contain NUL characters".into());
    }
    if value.len() > MAX_VALUE_LEN {
        return Err(format!(
            "Values are limited to {} KiB",
            MAX_VALUE_LEN / 1024
        ));
    }
    Ok(())
}

fn aad(project_id: &str, name: &str) -> Vec<u8> {
    let mut aad = Vec::with_capacity(project_id.len() + name.len() + 1);
    aad.extend_from_slice(project_id.as_bytes());
    aad.push(0);
    aad.extend_from_slice(name.as_bytes());
    aad
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{:02x}", b)).collect()
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
        .collect()
}

/// Reads the key file, creating it (mode 0600) with a fresh key if missing.
fn load_or_create_key(path: &Path, rng: &SystemRandom) -> Result<Vec<u8>, anyhow::Error> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let key = unhex(text.trim()).filter(|k| k.len() == KEY_LEN);
            return key.ok_or_else(|| {
                anyhow::anyhow!(
                    "{} is not a valid secrets key ({} hex characters expected)",
                    path.display(),
                    KEY_LEN * 2
                )
            });
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(anyhow::anyhow!("Cannot read {}: {}", path.display(), e)),
    }
    let mut key = vec![0u8; KEY_LEN];
    rng.fill(&mut key)
        .map_err(|_| anyhow::anyhow!("No secure random source"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|e| anyhow::anyhow!("Cannot create {}: {}", path.display(), e))?;
    file.write_all(format!("{}\n", hex(&key)).as_bytes())?;
    file.sync_all()?;
    Ok(key)
}

impl SecretStore {
    /// Opens the store on an initialized database (see
    /// `EventStore::initialize_db`), creating the key file on first use.
    pub async fn open(pool: SqlitePool, key_path: &Path) -> Result<Self, anyhow::Error> {
        let rng = SystemRandom::new();
        let key = load_or_create_key(key_path, &rng)?;
        let key = LessSafeKey::new(
            UnboundKey::new(&CHACHA20_POLY1305, &key)
                .map_err(|_| anyhow::anyhow!("Invalid secrets key"))?,
        );
        let mut cache: HashMap<String, BTreeMap<String, Sealed>> = HashMap::new();
        for row in sqlx::query("SELECT project_id, name, sealed, updated_at FROM secrets")
            .fetch_all(&pool)
            .await?
        {
            cache.entry(row.try_get("project_id")?).or_default().insert(
                row.try_get("name")?,
                Sealed {
                    bytes: row.try_get("sealed")?,
                    updated_at: row.try_get("updated_at")?,
                },
            );
        }
        let store = Self {
            inner: Arc::new(Inner {
                pool,
                key,
                rng,
                cache: Mutex::new(cache),
            }),
        };
        // Fail now, not at the next deploy, if the key file was replaced.
        for (project, names) in store.inner.cache.lock().unwrap().iter() {
            for (name, sealed) in names {
                store.open_value(project, name, sealed).map_err(|_| {
                    anyhow::anyhow!(
                        "Secret {} of project {} can't be decrypted with {}; was the key file replaced?",
                        name,
                        project,
                        key_path.display()
                    )
                })?;
            }
        }
        Ok(store)
    }

    fn seal(&self, project_id: &str, name: &str, value: &str) -> Result<Vec<u8>, anyhow::Error> {
        let mut nonce = [0u8; NONCE_LEN];
        self.inner
            .rng
            .fill(&mut nonce)
            .map_err(|_| anyhow::anyhow!("No secure random source"))?;
        let mut data = value.as_bytes().to_vec();
        self.inner
            .key
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(aad(project_id, name)),
                &mut data,
            )
            .map_err(|_| anyhow::anyhow!("Encryption failed"))?;
        let mut sealed = nonce.to_vec();
        sealed.extend_from_slice(&data);
        Ok(sealed)
    }

    fn open_value(&self, project_id: &str, name: &str, sealed: &Sealed) -> Result<String, ()> {
        if sealed.bytes.len() < NONCE_LEN {
            return Err(());
        }
        let (nonce, data) = sealed.bytes.split_at(NONCE_LEN);
        let nonce = Nonce::try_assume_unique_for_key(nonce).map_err(|_| ())?;
        let mut data = data.to_vec();
        let plain = self
            .inner
            .key
            .open_in_place(nonce, Aad::from(aad(project_id, name)), &mut data)
            .map_err(|_| ())?;
        String::from_utf8(plain.to_vec()).map_err(|_| ())
    }

    /// Creates or replaces a secret.
    pub async fn set(
        &self,
        project_id: &str,
        name: &str,
        value: &str,
    ) -> Result<(), anyhow::Error> {
        validate_name(name).map_err(anyhow::Error::msg)?;
        validate_value(value).map_err(anyhow::Error::msg)?;
        let sealed = Sealed {
            bytes: self.seal(project_id, name, value)?,
            updated_at: chrono::Utc::now().to_rfc3339(),
        };
        sqlx::query(
            "INSERT INTO secrets (project_id, name, sealed, updated_at) VALUES (?, ?, ?, ?)
             ON CONFLICT (project_id, name) DO UPDATE SET sealed = excluded.sealed, updated_at = excluded.updated_at",
        )
        .bind(project_id)
        .bind(name)
        .bind(&sealed.bytes)
        .bind(&sealed.updated_at)
        .execute(&self.inner.pool)
        .await?;
        self.inner
            .cache
            .lock()
            .unwrap()
            .entry(project_id.to_string())
            .or_default()
            .insert(name.to_string(), sealed);
        Ok(())
    }

    /// Deletes a secret; returns whether it existed.
    pub async fn remove(&self, project_id: &str, name: &str) -> Result<bool, anyhow::Error> {
        let result = sqlx::query("DELETE FROM secrets WHERE project_id = ? AND name = ?")
            .bind(project_id)
            .bind(name)
            .execute(&self.inner.pool)
            .await?;
        if let Some(names) = self.inner.cache.lock().unwrap().get_mut(project_id) {
            names.remove(name);
        }
        Ok(result.rows_affected() > 0)
    }

    /// A project's secret names, sorted.
    pub fn list(&self, project_id: &str) -> Vec<SecretInfo> {
        self.inner
            .cache
            .lock()
            .unwrap()
            .get(project_id)
            .map(|names| {
                names
                    .iter()
                    .map(|(name, s)| SecretInfo {
                        name: name.clone(),
                        updated_at: s.updated_at.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Decrypted values for a project's processes. Values that can't be
    /// decrypted are skipped (and reported by `open` at startup).
    pub fn env(&self, project_id: &str) -> BTreeMap<String, String> {
        let cache = self.inner.cache.lock().unwrap();
        let Some(names) = cache.get(project_id) else {
            return BTreeMap::new();
        };
        names
            .iter()
            .filter_map(|(name, sealed)| {
                Some((
                    name.clone(),
                    self.open_value(project_id, name, sealed).ok()?,
                ))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        aegis_event_store::EventStore::initialize_db(&pool)
            .await
            .unwrap();
        pool
    }

    #[tokio::test]
    async fn values_are_encrypted_and_survive_reopen() {
        let dir = tempfile::TempDir::new().unwrap();
        let key = dir.path().join("secrets.key");
        let pool = pool().await;
        let store = SecretStore::open(pool.clone(), &key).await.unwrap();
        store
            .set("p1", "DATABASE_URL", "postgres://u:hunter2@db/app")
            .await
            .unwrap();
        store.set("p1", "API_KEY", "k1").await.unwrap();
        store.set("p1", "API_KEY", "k2").await.unwrap();
        store.set("p2", "API_KEY", "other").await.unwrap();

        let names: Vec<_> = store.list("p1").into_iter().map(|s| s.name).collect();
        assert_eq!(names, vec!["API_KEY", "DATABASE_URL"]);
        assert_eq!(store.env("p1")["API_KEY"], "k2");
        assert_eq!(store.env("p2")["API_KEY"], "other");
        assert!(store.env("p3").is_empty());

        // The database holds no plaintext.
        let blobs: Vec<Vec<u8>> = sqlx::query_scalar("SELECT sealed FROM secrets")
            .fetch_all(&pool)
            .await
            .unwrap();
        for blob in blobs {
            assert!(!String::from_utf8_lossy(&blob).contains("hunter2"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&key).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        let reopened = SecretStore::open(pool.clone(), &key).await.unwrap();
        assert_eq!(
            reopened.env("p1")["DATABASE_URL"],
            "postgres://u:hunter2@db/app"
        );
        assert!(reopened.remove("p1", "API_KEY").await.unwrap());
        assert!(!reopened.remove("p1", "API_KEY").await.unwrap());
        assert_eq!(reopened.list("p1").len(), 1);

        // A different key can't read them, and says so at startup.
        let other_key = dir.path().join("other.key");
        let err = SecretStore::open(pool, &other_key).await.err().unwrap();
        assert!(err.to_string().contains("key file"), "{err}");
    }

    #[tokio::test]
    async fn sealed_values_are_bound_to_their_slot() {
        let dir = tempfile::TempDir::new().unwrap();
        let pool = pool().await;
        let store = SecretStore::open(pool.clone(), &dir.path().join("k"))
            .await
            .unwrap();
        store.set("p1", "A", "alpha").await.unwrap();
        // Copy p1's sealed A into p2's slot B: it must not decrypt there.
        sqlx::query("INSERT INTO secrets SELECT 'p2', 'B', sealed, updated_at FROM secrets WHERE name = 'A'")
            .execute(&pool)
            .await
            .unwrap();
        assert!(SecretStore::open(pool, &dir.path().join("k"))
            .await
            .is_err());
    }

    #[test]
    fn names_and_values_are_validated() {
        for ok in ["A", "_X", "DATABASE_URL", "a1"] {
            assert!(validate_name(ok).is_ok(), "{ok}");
        }
        for bad in ["", "1A", "A-B", "A B", "PORT", &"A".repeat(129)] {
            assert!(validate_name(bad).is_err(), "{bad}");
        }
        assert!(validate_value("x\0y").is_err());
        assert!(validate_value(&"x".repeat(MAX_VALUE_LEN + 1)).is_err());
        assert!(validate_value("multi\nline").is_ok());
        assert_eq!(unhex(&hex(&[0, 255, 16])), Some(vec![0, 255, 16]));
        assert_eq!(unhex("zz"), None);
    }

    #[test]
    fn corrupt_key_file_is_rejected() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("k");
        std::fs::write(&path, "abc").unwrap();
        assert!(load_or_create_key(&path, &SystemRandom::new()).is_err());
    }
}
