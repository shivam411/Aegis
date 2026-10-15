//! Automatic certificates over ACME (RFC 8555) with the HTTP-01 challenge.
//!
//! The CA asks for `http://<domain>/.well-known/acme-challenge/<token>`; the
//! proxy's HTTP listener answers from [`ChallengeStore`] while an order is
//! being validated. Issued certificates are stored under `certs/<domain>/`
//! (`key.pem` is mode 0600) and renewed when less than 30 days remain.

use crate::certs::{create_private_dir, write_private};
use instant_acme::{
    Account, AccountCredentials, AuthorizationStatus, ChallengeType, Identifier, NewAccount,
    NewOrder, OrderStatus, RetryPolicy,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::time::Duration;

pub const CHALLENGE_PREFIX: &str = "/.well-known/acme-challenge/";
pub const LETS_ENCRYPT_PRODUCTION: &str = "https://acme-v02.api.letsencrypt.org/directory";
pub const LETS_ENCRYPT_STAGING: &str = "https://acme-staging-v02.api.letsencrypt.org/directory";

/// ACME account settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcmeSettings {
    /// Contact address for the CA (expiry and policy notices).
    pub email: Option<String>,
    pub directory_url: String,
}

/// Pending HTTP-01 challenge responses, by token.
#[derive(Debug, Default)]
pub struct ChallengeStore {
    tokens: RwLock<HashMap<String, String>>,
}

impl ChallengeStore {
    pub fn insert(&self, token: &str, key_authorization: &str) {
        self.tokens
            .write()
            .unwrap()
            .insert(token.to_string(), key_authorization.to_string());
    }

    pub fn remove(&self, token: &str) {
        self.tokens.write().unwrap().remove(token);
    }

    /// The key authorization to answer for a request path, if the path is
    /// an ACME challenge for a token we are waiting on.
    pub fn respond(&self, path: &str) -> Option<String> {
        let token = path.strip_prefix(CHALLENGE_PREFIX)?;
        // Tokens are base64url (RFC 8555 §8.3).
        if token.is_empty()
            || !token
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return None;
        }
        self.tokens.read().unwrap().get(token).cloned()
    }

    pub fn len(&self) -> usize {
        self.tokens.read().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Removes a set of challenge tokens when dropped, so a failed order
/// doesn't leave them behind.
struct TokenCleanup<'a> {
    store: &'a ChallengeStore,
    tokens: Vec<String>,
}

impl Drop for TokenCleanup<'_> {
    fn drop(&mut self) {
        for token in &self.tokens {
            self.store.remove(token);
        }
    }
}

/// The account file for a directory: one account per CA directory, so
/// staging and production don't share credentials.
fn account_path(certs_dir: &Path, directory_url: &str) -> PathBuf {
    let name = match directory_url {
        LETS_ENCRYPT_PRODUCTION => "letsencrypt".to_string(),
        LETS_ENCRYPT_STAGING => "letsencrypt-staging".to_string(),
        other => {
            // FNV-1a: a stable, dependency-free name for custom directories.
            let hash = other.bytes().fold(0xcbf29ce484222325u64, |h, b| {
                (h ^ b as u64).wrapping_mul(0x100000001b3)
            });
            format!("{:016x}", hash)
        }
    };
    certs_dir.join(format!("acme-account-{}.json", name))
}

async fn account(settings: &AcmeSettings, certs_dir: &Path) -> anyhow::Result<Account> {
    let path = account_path(certs_dir, &settings.directory_url);
    if let Ok(saved) = std::fs::read(&path) {
        let credentials: AccountCredentials = serde_json::from_slice(&saved)?;
        return Ok(Account::builder()?.from_credentials(credentials).await?);
    }
    let contact = settings.email.as_ref().map(|e| format!("mailto:{}", e));
    let contacts: Vec<&str> = contact.iter().map(String::as_str).collect();
    let (account, credentials) = Account::builder()?
        .create(
            &NewAccount {
                contact: &contacts,
                terms_of_service_agreed: true,
                only_return_existing: false,
            },
            settings.directory_url.clone(),
            None,
        )
        .await?;
    create_private_dir(certs_dir)?;
    write_private(&path, &serde_json::to_vec(&credentials)?)?;
    tracing::info!(directory = %settings.directory_url, "Created ACME account");
    Ok(account)
}

/// Obtains a certificate for `domain`. Returns (certificate chain PEM,
/// private key PEM). The HTTP-01 challenge is answered by the proxy's HTTP
/// listener through `challenges`, so it must be reachable on port 80.
pub async fn issue(
    settings: &AcmeSettings,
    certs_dir: &Path,
    domain: &str,
    challenges: &ChallengeStore,
) -> anyhow::Result<(String, String)> {
    let account = account(settings, certs_dir).await?;
    let identifiers = [Identifier::Dns(domain.to_string())];
    let mut order = account.new_order(&NewOrder::new(&identifiers)).await?;
    let mut cleanup = TokenCleanup {
        store: challenges,
        tokens: Vec::new(),
    };

    let mut authorizations = order.authorizations();
    while let Some(result) = authorizations.next().await {
        // `authorizations` borrows `order` until this loop ends.
        let mut authz = result?;
        match authz.status {
            AuthorizationStatus::Pending => {}
            AuthorizationStatus::Valid => continue,
            other => anyhow::bail!("Authorization for {} is {:?}", domain, other),
        }
        let mut challenge = authz
            .challenge(ChallengeType::Http01)
            .ok_or_else(|| anyhow::anyhow!("The CA offered no HTTP-01 challenge"))?;
        let token = challenge.token.clone();
        challenges.insert(&token, challenge.key_authorization().as_str());
        cleanup.tokens.push(token);
        challenge.set_ready().await?;
    }

    let patience = RetryPolicy::new()
        .initial_delay(Duration::from_secs(1))
        .timeout(Duration::from_secs(120));
    let status = order.poll_ready(&patience).await?;
    if status != OrderStatus::Ready {
        let detail = order
            .state()
            .error
            .as_ref()
            .and_then(|p| p.detail.clone())
            .unwrap_or_default();
        anyhow::bail!(
            "The CA did not validate {} (order is {:?}). Check that its DNS points to this \
             server and port 80 is reachable. {}",
            domain,
            status,
            detail
        );
    }
    drop(cleanup);

    let key = rcgen::KeyPair::generate()?;
    let mut params = rcgen::CertificateParams::new(vec![domain.to_string()])?;
    params.distinguished_name = rcgen::DistinguishedName::new();
    let csr = params.serialize_request(&key)?;
    order.finalize_csr(csr.der()).await?;
    let chain = order.poll_certificate(&patience).await?;
    Ok((chain, key.serialize_pem()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_only_known_well_formed_tokens() {
        let store = ChallengeStore::default();
        assert!(store.is_empty());
        store.insert("tok_EN-123", "tok_EN-123.thumbprint");
        assert_eq!(
            store
                .respond("/.well-known/acme-challenge/tok_EN-123")
                .as_deref(),
            Some("tok_EN-123.thumbprint")
        );
        for path in [
            "/.well-known/acme-challenge/",
            "/.well-known/acme-challenge/other",
            "/.well-known/acme-challenge/tok_EN-123/x",
            "/.well-known/acme-challenge/../tok_EN-123",
            "/.well-known/acme-challenge/tok%5FEN-123",
            "/tok_EN-123",
        ] {
            assert_eq!(store.respond(path), None, "{path}");
        }
        store.remove("tok_EN-123");
        assert_eq!(
            store.respond("/.well-known/acme-challenge/tok_EN-123"),
            None
        );
    }

    #[test]
    fn cleanup_removes_tokens_on_drop() {
        let store = ChallengeStore::default();
        store.insert("a", "a.x");
        store.insert("b", "b.x");
        {
            let _cleanup = TokenCleanup {
                store: &store,
                tokens: vec!["a".into()],
            };
        }
        assert_eq!(store.len(), 1);
        assert!(store.respond("/.well-known/acme-challenge/b").is_some());
    }

    #[test]
    fn account_files_are_per_directory() {
        let dir = Path::new("/var/lib/aegis/certs");
        let prod = account_path(dir, LETS_ENCRYPT_PRODUCTION);
        let staging = account_path(dir, LETS_ENCRYPT_STAGING);
        let custom = account_path(dir, "https://acme.example/dir");
        assert_ne!(prod, staging);
        assert_ne!(prod, custom);
        assert_eq!(custom, account_path(dir, "https://acme.example/dir"));
        assert!(prod.ends_with("acme-account-letsencrypt.json"));
    }
}
