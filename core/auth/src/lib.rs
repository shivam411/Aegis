//! Authentication for the HTTP API: the admin account, browser sessions,
//! API tokens, login throttling and webhook secrets.
//!
//! Secrets are generated from the OS RNG and stored hashed: passwords with
//! Argon2id, and session ids and API tokens (256 random bits each) with
//! SHA-256. Plaintext is returned exactly once, when it is created.

use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use rand::rngs::OsRng;
use rand::RngCore;
use sha2::{Digest, Sha256};
use sqlx::{Row, SqlitePool};
use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const ADMIN_USER: &str = "admin";
pub const MIN_PASSWORD_LEN: usize = 12;
const TOKEN_PREFIX: &str = "aegis_";
/// Don't rewrite `last_seen` on every request.
const TOUCH_INTERVAL_SECS: i64 = 60;

fn random_string(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    OsRng.fill_bytes(&mut buf);
    URL_SAFE_NO_PAD.encode(buf)
}

/// SHA-256 hex digest; used to store high-entropy secrets.
pub fn sha256_hex(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

fn now_secs() -> i64 {
    chrono::Utc::now().timestamp()
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn hash_password(password: &str) -> Result<String, anyhow::Error> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| anyhow::anyhow!("Password hashing failed: {}", e))
}

fn verify_password(password: &str, hash: &str) -> bool {
    PasswordHash::new(hash)
        .map(|parsed| {
            Argon2::default()
                .verify_password(password.as_bytes(), &parsed)
                .is_ok()
        })
        .unwrap_or(false)
}

#[derive(Debug, Clone)]
pub struct AuthConfig {
    /// A session ends after this long without requests.
    pub session_idle: Duration,
    /// A session ends this long after login, however active.
    pub session_absolute: Duration,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            session_idle: Duration::from_secs(2 * 3600),
            session_absolute: Duration::from_secs(24 * 3600),
        }
    }
}

/// What an API token may do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenScope {
    /// Read-only requests.
    Read,
    /// Everything except managing tokens and passwords.
    Deploy,
}

impl TokenScope {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Deploy => "deploy",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "read" => Some(Self::Read),
            "deploy" => Some(Self::Deploy),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct NewSession {
    /// Cookie value; not stored.
    pub token: String,
    pub csrf_token: String,
    pub expires_at: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Session {
    pub username: String,
    pub csrf_token: String,
    pub created_at: i64,
    /// When the session will expire if it stays idle (unix seconds).
    pub expires_at: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TokenInfo {
    pub id: String,
    pub name: String,
    pub scope: TokenScope,
    pub created_at: String,
    pub last_used_at: Option<String>,
    pub revoked: bool,
}

#[derive(Debug, Clone)]
pub struct NewToken {
    pub info: TokenInfo,
    /// Shown once; only its hash is stored.
    pub token: String,
}

#[derive(Clone)]
pub struct AuthStore {
    pool: SqlitePool,
    config: AuthConfig,
    /// Verified against when the user doesn't exist, so response time
    /// doesn't reveal which usernames exist.
    dummy_hash: std::sync::Arc<String>,
    /// Each Argon2 check uses ~19 MiB; cap how many run at once so a flood
    /// of sign-in attempts can't exhaust memory.
    verify_permits: std::sync::Arc<tokio::sync::Semaphore>,
}

/// Concurrent password checks allowed at once.
const MAX_CONCURRENT_PASSWORD_CHECKS: usize = 4;

impl AuthStore {
    pub fn new(pool: SqlitePool, config: AuthConfig) -> Result<Self, anyhow::Error> {
        Ok(Self {
            pool,
            config,
            dummy_hash: std::sync::Arc::new(hash_password(&random_string(16))?),
            verify_permits: std::sync::Arc::new(tokio::sync::Semaphore::new(
                MAX_CONCURRENT_PASSWORD_CHECKS,
            )),
        })
    }

    pub fn config(&self) -> &AuthConfig {
        &self.config
    }

    pub async fn has_users(&self) -> Result<bool, anyhow::Error> {
        let row = sqlx::query("SELECT COUNT(*) FROM users")
            .fetch_one(&self.pool)
            .await?;
        Ok(row.try_get::<i64, _>(0)? > 0)
    }

    /// Creates the `admin` account with a random password when no account
    /// exists yet, and returns that password. Returns `None` otherwise.
    pub async fn bootstrap_admin(&self) -> Result<Option<String>, anyhow::Error> {
        if self.has_users().await? {
            return Ok(None);
        }
        let password = random_string(18);
        let hash = hash_blocking(password.clone()).await?;
        let now = now_rfc3339();
        sqlx::query(
            "INSERT INTO users (username, password_hash, created_at, password_changed_at) VALUES (?, ?, ?, ?)",
        )
        .bind(ADMIN_USER)
        .bind(hash)
        .bind(&now)
        .bind(&now)
        .execute(&self.pool)
        .await?;
        Ok(Some(password))
    }

    /// Sets a user's password (creating the user if needed) and ends all of
    /// their sessions.
    pub async fn set_password(&self, username: &str, password: &str) -> Result<(), anyhow::Error> {
        if password.chars().count() < MIN_PASSWORD_LEN {
            anyhow::bail!("Password must be at least {} characters", MIN_PASSWORD_LEN);
        }
        let hash = hash_blocking(password.to_string()).await?;
        let now = now_rfc3339();
        sqlx::query(
            "INSERT INTO users (username, password_hash, created_at, password_changed_at) VALUES (?, ?, ?, ?)
             ON CONFLICT(username) DO UPDATE SET password_hash = excluded.password_hash, password_changed_at = excluded.password_changed_at",
        )
        .bind(username)
        .bind(hash)
        .bind(&now)
        .bind(&now)
        .execute(&self.pool)
        .await?;
        sqlx::query("DELETE FROM sessions WHERE username = ?")
            .bind(username)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Checks a username/password pair in constant-ish time.
    pub async fn verify_login(
        &self,
        username: &str,
        password: &str,
    ) -> Result<bool, anyhow::Error> {
        let stored: Option<String> =
            sqlx::query("SELECT password_hash FROM users WHERE username = ?")
                .bind(username)
                .fetch_optional(&self.pool)
                .await?
                .map(|row| row.try_get(0))
                .transpose()?;
        let exists = stored.is_some();
        let hash = stored.unwrap_or_else(|| self.dummy_hash.as_ref().clone());
        let password = password.to_string();
        let _permit = self.verify_permits.acquire().await?;
        let ok = tokio::task::spawn_blocking(move || verify_password(&password, &hash)).await?;
        Ok(exists && ok)
    }

    pub async fn user_exists(&self, username: &str) -> Result<bool, anyhow::Error> {
        Ok(sqlx::query("SELECT 1 FROM users WHERE username = ?")
            .bind(username)
            .fetch_optional(&self.pool)
            .await?
            .is_some())
    }

    pub async fn create_session(&self, username: &str) -> Result<NewSession, anyhow::Error> {
        let token = random_string(32);
        let csrf_token = random_string(32);
        let now = now_secs();
        sqlx::query(
            "INSERT INTO sessions (id_hash, username, csrf_token, created_at, last_seen) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(sha256_hex(&token))
        .bind(username)
        .bind(&csrf_token)
        .bind(now)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(NewSession {
            token,
            csrf_token,
            expires_at: self.expiry(now, now),
        })
    }

    fn expiry(&self, created_at: i64, last_seen: i64) -> i64 {
        (last_seen + self.config.session_idle.as_secs() as i64)
            .min(created_at + self.config.session_absolute.as_secs() as i64)
    }

    /// Looks up a live session, extending its idle timeout. Expired sessions
    /// are deleted and reported as `None`.
    pub async fn session(&self, token: &str) -> Result<Option<Session>, anyhow::Error> {
        let id_hash = sha256_hex(token);
        let Some(row) = sqlx::query(
            "SELECT username, csrf_token, created_at, last_seen FROM sessions WHERE id_hash = ?",
        )
        .bind(&id_hash)
        .fetch_optional(&self.pool)
        .await?
        else {
            return Ok(None);
        };
        let (username, csrf_token, created_at, last_seen): (String, String, i64, i64) = (
            row.try_get(0)?,
            row.try_get(1)?,
            row.try_get(2)?,
            row.try_get(3)?,
        );
        let now = now_secs();
        if now >= self.expiry(created_at, last_seen) {
            sqlx::query("DELETE FROM sessions WHERE id_hash = ?")
                .bind(&id_hash)
                .execute(&self.pool)
                .await?;
            return Ok(None);
        }
        let mut last_seen = last_seen;
        if now - last_seen >= TOUCH_INTERVAL_SECS {
            sqlx::query("UPDATE sessions SET last_seen = ? WHERE id_hash = ?")
                .bind(now)
                .bind(&id_hash)
                .execute(&self.pool)
                .await?;
            last_seen = now;
        }
        Ok(Some(Session {
            username,
            csrf_token,
            created_at,
            expires_at: self.expiry(created_at, last_seen),
        }))
    }

    pub async fn delete_session(&self, token: &str) -> Result<(), anyhow::Error> {
        sqlx::query("DELETE FROM sessions WHERE id_hash = ?")
            .bind(sha256_hex(token))
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Removes expired sessions; returns how many were removed.
    pub async fn purge_expired_sessions(&self) -> Result<u64, anyhow::Error> {
        let now = now_secs();
        let res =
            sqlx::query("DELETE FROM sessions WHERE last_seen + ? <= ? OR created_at + ? <= ?")
                .bind(self.config.session_idle.as_secs() as i64)
                .bind(now)
                .bind(self.config.session_absolute.as_secs() as i64)
                .bind(now)
                .execute(&self.pool)
                .await?;
        Ok(res.rows_affected())
    }

    pub async fn create_token(
        &self,
        name: &str,
        scope: TokenScope,
    ) -> Result<NewToken, anyhow::Error> {
        let name = name.trim();
        if name.is_empty() || name.len() > 64 {
            anyhow::bail!("Token name must be 1-64 characters");
        }
        let id = aegis_types::EventId::new().to_string();
        let token = format!("{}{}", TOKEN_PREFIX, random_string(32));
        let created_at = now_rfc3339();
        sqlx::query(
            "INSERT INTO api_tokens (id, name, token_hash, scope, created_at) VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&id)
        .bind(name)
        .bind(sha256_hex(&token))
        .bind(scope.as_str())
        .bind(&created_at)
        .execute(&self.pool)
        .await?;
        Ok(NewToken {
            info: TokenInfo {
                id,
                name: name.to_string(),
                scope,
                created_at,
                last_used_at: None,
                revoked: false,
            },
            token,
        })
    }

    fn token_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<TokenInfo, anyhow::Error> {
        let scope: String = row.try_get("scope")?;
        Ok(TokenInfo {
            id: row.try_get("id")?,
            name: row.try_get("name")?,
            scope: TokenScope::parse(&scope).unwrap_or(TokenScope::Read),
            created_at: row.try_get("created_at")?,
            last_used_at: row.try_get("last_used_at")?,
            revoked: row.try_get::<Option<String>, _>("revoked_at")?.is_some(),
        })
    }

    pub async fn list_tokens(&self) -> Result<Vec<TokenInfo>, anyhow::Error> {
        let rows = sqlx::query(
            "SELECT id, name, scope, created_at, last_used_at, revoked_at FROM api_tokens ORDER BY created_at",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(Self::token_from_row).collect()
    }

    /// Returns false if no active token has this id.
    pub async fn revoke_token(&self, id: &str) -> Result<bool, anyhow::Error> {
        let res =
            sqlx::query("UPDATE api_tokens SET revoked_at = ? WHERE id = ? AND revoked_at IS NULL")
                .bind(now_rfc3339())
                .bind(id)
                .execute(&self.pool)
                .await?;
        Ok(res.rows_affected() > 0)
    }

    /// The active token matching `token`, if any.
    pub async fn authenticate_token(
        &self,
        token: &str,
    ) -> Result<Option<TokenInfo>, anyhow::Error> {
        if !token.starts_with(TOKEN_PREFIX) {
            return Ok(None);
        }
        let hash = sha256_hex(token);
        let Some(row) = sqlx::query(
            "SELECT id, name, scope, created_at, last_used_at, revoked_at FROM api_tokens WHERE token_hash = ? AND revoked_at IS NULL",
        )
        .bind(&hash)
        .fetch_optional(&self.pool)
        .await?
        else {
            return Ok(None);
        };
        let info = Self::token_from_row(&row)?;
        sqlx::query("UPDATE api_tokens SET last_used_at = ? WHERE id = ?")
            .bind(now_rfc3339())
            .bind(&info.id)
            .execute(&self.pool)
            .await?;
        Ok(Some(info))
    }

    pub async fn webhook_secret(&self, project_id: &str) -> Result<Option<String>, anyhow::Error> {
        Ok(
            sqlx::query("SELECT secret FROM webhook_secrets WHERE project_id = ?")
                .bind(project_id)
                .fetch_optional(&self.pool)
                .await?
                .map(|row| row.try_get(0))
                .transpose()?,
        )
    }

    /// Creates or replaces a project's webhook secret and returns it.
    pub async fn rotate_webhook_secret(&self, project_id: &str) -> Result<String, anyhow::Error> {
        let secret = random_string(32);
        sqlx::query(
            "INSERT INTO webhook_secrets (project_id, secret, created_at) VALUES (?, ?, ?)
             ON CONFLICT(project_id) DO UPDATE SET secret = excluded.secret, created_at = excluded.created_at",
        )
        .bind(project_id)
        .bind(&secret)
        .bind(now_rfc3339())
        .execute(&self.pool)
        .await?;
        Ok(secret)
    }
}

async fn hash_blocking(password: String) -> Result<String, anyhow::Error> {
    tokio::task::spawn_blocking(move || hash_password(&password)).await?
}

/// Throttles password guessing: after `max_failures` failed logins for a
/// key (client address or username) within `window`, that key is locked
/// out for `lockout`.
pub struct LoginLimiter {
    max_failures: usize,
    window: Duration,
    lockout: Duration,
    state: Mutex<HashMap<String, KeyState>>,
}

#[derive(Default)]
struct KeyState {
    failures: VecDeque<Instant>,
    locked_until: Option<Instant>,
}

impl Default for LoginLimiter {
    fn default() -> Self {
        Self::new(
            5,
            Duration::from_secs(15 * 60),
            Duration::from_secs(15 * 60),
        )
    }
}

impl LoginLimiter {
    pub fn new(max_failures: usize, window: Duration, lockout: Duration) -> Self {
        Self {
            max_failures,
            window,
            lockout,
            state: Mutex::new(HashMap::new()),
        }
    }

    /// `Err(retry_after)` if any key is locked out.
    pub fn check(&self, keys: &[String]) -> Result<(), Duration> {
        let now = Instant::now();
        let state = self.state.lock().unwrap();
        let wait = keys
            .iter()
            .filter_map(|k| state.get(k)?.locked_until)
            .filter(|until| *until > now)
            .map(|until| until - now)
            .max();
        match wait {
            Some(wait) => Err(wait),
            None => Ok(()),
        }
    }

    /// Counts an attempt for `key` before it is checked, atomically with the
    /// lockout test, so parallel attempts can't all slip through. Returns
    /// `Err(retry_after)` if the key is locked out (nothing is counted).
    pub fn try_begin(&self, key: &str) -> Result<(), Duration> {
        let now = Instant::now();
        let mut state = self.state.lock().unwrap();
        // Bound memory under a flood of distinct keys.
        if state.len() > 10_000 {
            state.retain(|_, s| s.locked_until.is_some_and(|u| u > now));
        }
        let entry = state.entry(key.to_string()).or_default();
        if let Some(until) = entry.locked_until.filter(|until| *until > now) {
            return Err(until - now);
        }
        while entry
            .failures
            .front()
            .is_some_and(|t| now.duration_since(*t) > self.window)
        {
            entry.failures.pop_front();
        }
        entry.failures.push_back(now);
        if entry.failures.len() >= self.max_failures {
            entry.locked_until = Some(now + self.lockout);
            entry.failures.clear();
        }
        Ok(())
    }

    pub fn record_failure(&self, keys: &[String]) {
        for key in keys {
            let _ = self.try_begin(key);
        }
    }

    pub fn record_success(&self, keys: &[String]) {
        let mut state = self.state.lock().unwrap();
        for key in keys {
            state.remove(key);
        }
    }
}

/// Login throttling with separate limits per client address and per
/// username. The per-username limit is deliberately higher, so a single
/// attacker can't lock the real admin out as easily, while guessing a
/// random password stays hopeless either way.
pub struct LoginThrottle {
    pub by_client: LoginLimiter,
    pub by_user: LoginLimiter,
}

impl Default for LoginThrottle {
    fn default() -> Self {
        let window = Duration::from_secs(15 * 60);
        Self {
            by_client: LoginLimiter::new(5, window, window),
            by_user: LoginLimiter::new(20, window, window),
        }
    }
}

impl LoginThrottle {
    /// Reserves a sign-in attempt for this client and username. Call before
    /// checking the password and [`LoginThrottle::record_success`] after a
    /// correct one; a failed attempt simply stays counted.
    /// `Err(retry_after)` when either is locked out.
    pub fn begin(&self, client: &str, username: &str) -> Result<(), Duration> {
        self.by_client.try_begin(client)?;
        self.by_user.try_begin(&username.to_lowercase())
    }

    pub fn record_success(&self, client: &str, username: &str) {
        self.by_client.record_success(&[client.to_string()]);
        self.by_user.record_success(&[username.to_lowercase()]);
    }
}

/// Constant-time string comparison (for CSRF tokens and signatures).
pub fn constant_time_eq(a: &str, b: &str) -> bool {
    use subtle::ConstantTimeEq;
    a.len() == b.len() && bool::from(a.as_bytes().ct_eq(b.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    async fn store(config: AuthConfig) -> AuthStore {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        aegis_event_store::EventStore::initialize_db(&pool)
            .await
            .unwrap();
        AuthStore::new(pool, config).unwrap()
    }

    #[tokio::test]
    async fn test_bootstrap_login_and_password_change() {
        let auth = store(AuthConfig::default()).await;
        assert!(!auth.has_users().await.unwrap());
        let password = auth.bootstrap_admin().await.unwrap().unwrap();
        assert!(password.len() >= 20);
        assert!(auth.bootstrap_admin().await.unwrap().is_none(), "only once");

        assert!(auth.verify_login(ADMIN_USER, &password).await.unwrap());
        assert!(!auth
            .verify_login(ADMIN_USER, "wrong-password")
            .await
            .unwrap());
        assert!(!auth.verify_login("nobody", &password).await.unwrap());

        // The hash is Argon2id, never the plaintext.
        let row = sqlx::query("SELECT password_hash FROM users")
            .fetch_one(&auth.pool)
            .await
            .unwrap();
        let stored: String = row.get(0);
        assert!(stored.starts_with("$argon2id$"));
        assert!(!stored.contains(&password));

        let session = auth.create_session(ADMIN_USER).await.unwrap();
        assert!(auth.session(&session.token).await.unwrap().is_some());
        assert!(auth.set_password(ADMIN_USER, "short").await.is_err());
        auth.set_password(ADMIN_USER, "a-much-longer-password")
            .await
            .unwrap();
        assert!(
            auth.session(&session.token).await.unwrap().is_none(),
            "password change ends sessions"
        );
        assert!(auth
            .verify_login(ADMIN_USER, "a-much-longer-password")
            .await
            .unwrap());
    }

    #[tokio::test]
    async fn test_sessions_are_hashed_and_expire() {
        let auth = store(AuthConfig {
            session_idle: Duration::from_secs(1),
            session_absolute: Duration::from_secs(60),
        })
        .await;
        auth.set_password(ADMIN_USER, "correct-horse-battery")
            .await
            .unwrap();
        let s = auth.create_session(ADMIN_USER).await.unwrap();
        let found = auth.session(&s.token).await.unwrap().unwrap();
        assert_eq!(found.username, ADMIN_USER);
        assert_eq!(found.csrf_token, s.csrf_token);
        let stored: String = sqlx::query("SELECT id_hash FROM sessions")
            .fetch_one(&auth.pool)
            .await
            .unwrap()
            .get(0);
        assert_eq!(stored, sha256_hex(&s.token));
        assert!(auth.session("not-a-session").await.unwrap().is_none());

        tokio::time::sleep(Duration::from_millis(2100)).await;
        assert!(
            auth.session(&s.token).await.unwrap().is_none(),
            "idle expiry"
        );

        let auth = store(AuthConfig {
            session_idle: Duration::from_secs(60),
            session_absolute: Duration::from_secs(1),
        })
        .await;
        auth.set_password(ADMIN_USER, "correct-horse-battery")
            .await
            .unwrap();
        let s = auth.create_session(ADMIN_USER).await.unwrap();
        tokio::time::sleep(Duration::from_millis(2100)).await;
        assert!(
            auth.session(&s.token).await.unwrap().is_none(),
            "absolute expiry"
        );

        let s2 = auth.create_session(ADMIN_USER).await.unwrap();
        auth.delete_session(&s2.token).await.unwrap();
        assert!(auth.session(&s2.token).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_api_tokens_and_webhook_secrets() {
        let auth = store(AuthConfig::default()).await;
        let t = auth.create_token("ci", TokenScope::Deploy).await.unwrap();
        assert!(t.token.starts_with("aegis_"));
        let info = auth.authenticate_token(&t.token).await.unwrap().unwrap();
        assert_eq!(info.scope, TokenScope::Deploy);
        assert!(auth
            .authenticate_token("aegis_nope")
            .await
            .unwrap()
            .is_none());
        assert!(auth.authenticate_token("garbage").await.unwrap().is_none());
        assert!(auth.create_token("", TokenScope::Read).await.is_err());

        let listed = auth.list_tokens().await.unwrap();
        assert_eq!(listed.len(), 1);
        assert!(listed[0].last_used_at.is_some());
        let raw: String = sqlx::query("SELECT token_hash FROM api_tokens")
            .fetch_one(&auth.pool)
            .await
            .unwrap()
            .get(0);
        assert_ne!(raw, t.token);

        assert!(auth.revoke_token(&t.info.id).await.unwrap());
        assert!(!auth.revoke_token(&t.info.id).await.unwrap());
        assert!(auth.authenticate_token(&t.token).await.unwrap().is_none());
        assert!(auth.list_tokens().await.unwrap()[0].revoked);

        assert!(auth.webhook_secret("p1").await.unwrap().is_none());
        let s1 = auth.rotate_webhook_secret("p1").await.unwrap();
        assert_eq!(auth.webhook_secret("p1").await.unwrap(), Some(s1.clone()));
        let s2 = auth.rotate_webhook_secret("p1").await.unwrap();
        assert_ne!(s1, s2);
    }

    #[test]
    fn test_login_limiter_locks_out_and_recovers() {
        let limiter = LoginLimiter::new(3, Duration::from_secs(60), Duration::from_millis(200));
        let keys = vec!["ip:1.2.3.4".to_string(), "user:admin".to_string()];
        for _ in 0..2 {
            limiter.record_failure(&keys);
            assert!(limiter.check(&keys).is_ok());
        }
        limiter.record_failure(&keys);
        assert!(limiter.check(&keys).is_err());
        // The username is locked from any address.
        assert!(limiter
            .check(&["ip:5.6.7.8".into(), "user:admin".into()])
            .is_err());
        std::thread::sleep(Duration::from_millis(250));
        assert!(limiter.check(&keys).is_ok());

        limiter.record_failure(&keys);
        limiter.record_success(&keys);
        limiter.record_failure(&keys);
        limiter.record_failure(&keys);
        assert!(limiter.check(&keys).is_ok(), "success resets the count");
    }

    #[test]
    fn test_throttle_separates_clients_from_usernames() {
        let t = LoginThrottle::default();
        for _ in 0..5 {
            assert!(t.begin("203.0.113.9", "admin").is_ok());
        }
        assert!(t.begin("203.0.113.9", "admin").is_err(), "attacker blocked");
        assert!(
            t.begin("198.51.100.7", "Admin").is_ok(),
            "the real admin elsewhere is not locked out by one attacker"
        );
        t.record_success("198.51.100.7", "admin");
        for i in 0..20 {
            let _ = t.begin(&format!("192.0.2.{i}"), "admin");
        }
        assert!(
            t.begin("198.51.100.7", "admin").is_err(),
            "a distributed attack still locks the username"
        );
    }

    #[test]
    fn test_parallel_attempts_cannot_exceed_the_limit() {
        let t = std::sync::Arc::new(LoginThrottle::default());
        let allowed: usize = std::thread::scope(|scope| {
            (0..50)
                .map(|_| {
                    let t = t.clone();
                    scope.spawn(move || t.begin("203.0.113.9", "admin").is_ok())
                })
                .collect::<Vec<_>>()
                .into_iter()
                .map(|h| h.join().unwrap() as usize)
                .sum()
        });
        assert_eq!(allowed, 5);
    }

    #[test]
    fn test_constant_time_eq() {
        assert!(constant_time_eq("abc", "abc"));
        assert!(!constant_time_eq("abc", "abd"));
        assert!(!constant_time_eq("abc", "abcd"));
    }
}
