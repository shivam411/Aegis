//! Certificates for HTTPS: loading PEM files, choosing a certificate by SNI,
//! expiry parsing and the renewal decision.

use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use serde::Serialize;
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::SystemTime;

/// Renew certificates this long before they expire.
pub const RENEW_BEFORE_DAYS: i64 = 30;

/// Whether a certificate expiring at `not_after` should be (re)issued now.
/// A missing or unreadable expiry counts as "renew".
pub fn needs_renewal(not_after: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
    match not_after {
        None => true,
        Some(expiry) => expiry - now < chrono::Duration::days(RENEW_BEFORE_DAYS),
    }
}

/// A certificate ready to serve.
#[derive(Clone)]
pub struct LoadedCert {
    pub key: Arc<CertifiedKey>,
    pub not_after: Option<DateTime<Utc>>,
    /// Modification times of the files it was loaded from.
    pub stamps: (Option<SystemTime>, Option<SystemTime>),
}

impl std::fmt::Debug for LoadedCert {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoadedCert")
            .field("not_after", &self.not_after)
            .finish()
    }
}

pub fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Loads a certificate chain and its private key (PEM) and checks that they
/// belong together.
pub fn load_cert_files(cert_path: &Path, key_path: &Path) -> Result<LoadedCert, anyhow::Error> {
    let stamps = (modified(cert_path), modified(key_path));
    let cert_pem = std::fs::read(cert_path)
        .map_err(|e| anyhow::anyhow!("Cannot read {}: {}", cert_path.display(), e))?;
    let key_pem = std::fs::read(key_path)
        .map_err(|e| anyhow::anyhow!("Cannot read {}: {}", key_path.display(), e))?;
    let mut cert = load_cert_pem(&cert_pem, &key_pem)?;
    cert.stamps = stamps;
    Ok(cert)
}

/// Like [`load_cert_files`], from PEM bytes.
pub fn load_cert_pem(cert_pem: &[u8], key_pem: &[u8]) -> Result<LoadedCert, anyhow::Error> {
    let certs = rustls_pemfile::certs(&mut &cert_pem[..]).collect::<Result<Vec<_>, _>>()?;
    let Some(leaf) = certs.first() else {
        anyhow::bail!("No certificate found");
    };
    let not_after = not_after(leaf.as_ref());
    let key = rustls_pemfile::private_key(&mut &key_pem[..])?
        .ok_or_else(|| anyhow::anyhow!("No private key found"))?;
    let signing = rustls::crypto::ring::sign::any_supported_type(&key)
        .map_err(|e| anyhow::anyhow!("Unsupported private key: {}", e))?;
    let key = CertifiedKey::new(certs, signing);
    key.keys_match()
        .map_err(|e| anyhow::anyhow!("Certificate and private key don't match: {}", e))?;
    Ok(LoadedCert {
        key: Arc::new(key),
        not_after,
        stamps: (None, None),
    })
}

/// Reads one DER TLV: (tag, contents, rest).
fn der_tlv(input: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, rest) = input.split_first()?;
    let (&first, rest) = rest.split_first()?;
    let (len, rest) = if first < 0x80 {
        (first as usize, rest)
    } else {
        let n = (first & 0x7f) as usize;
        if n == 0 || n > 4 || rest.len() < n {
            return None;
        }
        let len = rest[..n]
            .iter()
            .fold(0usize, |acc, &b| (acc << 8) | b as usize);
        (len, &rest[n..])
    };
    if rest.len() < len {
        return None;
    }
    Some((tag, &rest[..len], &rest[len..]))
}

fn der_time(tag: u8, value: &[u8]) -> Option<DateTime<Utc>> {
    let text = std::str::from_utf8(value).ok()?;
    let text = text.strip_suffix('Z')?;
    let full = match tag {
        // UTCTime: YYMMDDHHMMSS; years 50-99 are 19xx.
        0x17 => {
            let yy: u32 = text.get(..2)?.parse().ok()?;
            format!("{}{}", if yy >= 50 { 19 } else { 20 }, text)
        }
        // GeneralizedTime: YYYYMMDDHHMMSS.
        0x18 => text.to_string(),
        _ => return None,
    };
    let naive = NaiveDateTime::parse_from_str(full.get(..14)?, "%Y%m%d%H%M%S").ok()?;
    Some(Utc.from_utc_datetime(&naive))
}

/// The `notAfter` time of a DER X.509 certificate.
pub fn not_after(der: &[u8]) -> Option<DateTime<Utc>> {
    let (_, cert, _) = der_tlv(der)?;
    let (_, tbs, _) = der_tlv(cert)?;
    let mut rest = tbs;
    let (tag, _, after) = der_tlv(rest)?;
    if tag == 0xa0 {
        // Explicit version.
        rest = after;
    }
    // serialNumber, signature, issuer.
    for _ in 0..3 {
        rest = der_tlv(rest)?.2;
    }
    let (_, validity, _) = der_tlv(rest)?;
    let (_, _, rest) = der_tlv(validity)?;
    let (tag, value, _) = der_tlv(rest)?;
    der_time(tag, value)
}

/// Writes `content` to `path` readable only by this user (on Unix), by
/// writing a temporary file and renaming it into place.
pub fn write_private(path: &Path, content: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    {
        let mut file = options.open(&tmp)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        }
        file.write_all(content)?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

/// Creates a directory that only this user can enter (on Unix).
pub fn create_private_dir(path: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Where an ACME-issued certificate for `domain` is kept.
pub fn acme_paths(certs_dir: &Path, domain: &str) -> (PathBuf, PathBuf) {
    let dir = certs_dir.join(domain);
    (dir.join("cert.pem"), dir.join("key.pem"))
}

/// The certificate state of one domain, for the API and dashboard.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CertStatus {
    pub domain: String,
    /// "acme", "files" or "off".
    pub mode: String,
    /// "valid", "expiring" (renewal due or failing), "expired", "pending"
    /// (being issued), "error", "disabled" (ACME is off) or "off" (HTTP only).
    pub state: String,
    /// RFC 3339 expiry of the served certificate.
    pub not_after: Option<String>,
    pub message: Option<String>,
}

/// The certificates being served, by domain. Also the rustls SNI resolver.
#[derive(Default)]
pub struct CertStore {
    certs: RwLock<HashMap<String, LoadedCert>>,
}

impl std::fmt::Debug for CertStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CertStore").finish()
    }
}

impl CertStore {
    pub fn insert(&self, domain: &str, cert: LoadedCert) {
        self.certs.write().unwrap().insert(domain.to_string(), cert);
    }

    pub fn remove(&self, domain: &str) {
        self.certs.write().unwrap().remove(domain);
    }

    pub fn get(&self, domain: &str) -> Option<LoadedCert> {
        self.certs.read().unwrap().get(domain).cloned()
    }

    pub fn contains(&self, domain: &str) -> bool {
        self.certs.read().unwrap().contains_key(domain)
    }
}

/// Picks the certificate for the server name the client asked for (SNI).
/// Clients that send no name, or a name without a certificate, fail the
/// handshake rather than getting another site's certificate.
#[derive(Debug)]
pub struct SniResolver(pub Arc<CertStore>);

impl ResolvesServerCert for SniResolver {
    fn resolve(&self, hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        let name = hello
            .server_name()?
            .trim_end_matches('.')
            .to_ascii_lowercase();
        self.0.get(&name).map(|c| c.key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn self_signed(domain: &str, expires: (i32, u8, u8)) -> (String, String) {
        let key = rcgen::KeyPair::generate().unwrap();
        let mut params = rcgen::CertificateParams::new(vec![domain.to_string()]).unwrap();
        params.not_after = rcgen::date_time_ymd(expires.0, expires.1, expires.2);
        let cert = params.self_signed(&key).unwrap();
        (cert.pem(), key.serialize_pem())
    }

    #[test]
    fn parses_not_after_from_der() {
        for (year, month, day) in [(2031, 7, 9), (2049, 12, 31), (2051, 1, 2)] {
            let (cert_pem, key_pem) = self_signed("a.example.com", (year, month, day));
            let loaded = load_cert_pem(cert_pem.as_bytes(), key_pem.as_bytes()).unwrap();
            let expiry = loaded.not_after.expect("expiry parsed");
            assert_eq!(
                expiry.format("%Y-%m-%d").to_string(),
                format!("{year:04}-{month:02}-{day:02}")
            );
        }
        assert_eq!(not_after(b"not der"), None);
        assert_eq!(not_after(&[]), None);
    }

    #[test]
    fn renewal_decision() {
        let now = Utc.with_ymd_and_hms(2026, 10, 1, 0, 0, 0).unwrap();
        assert!(needs_renewal(None, now));
        assert!(needs_renewal(Some(now - chrono::Duration::days(1)), now));
        assert!(needs_renewal(Some(now + chrono::Duration::days(29)), now));
        assert!(needs_renewal(
            Some(now + chrono::Duration::days(30) - chrono::Duration::seconds(1)),
            now
        ));
        assert!(!needs_renewal(Some(now + chrono::Duration::days(30)), now));
        assert!(!needs_renewal(Some(now + chrono::Duration::days(89)), now));
    }

    #[test]
    fn rejects_mismatched_pairs() {
        let (cert_a, _) = self_signed("a.example.com", (2031, 1, 1));
        let (_, key_b) = self_signed("b.example.com", (2031, 1, 1));
        let err = load_cert_pem(cert_a.as_bytes(), key_b.as_bytes()).unwrap_err();
        assert!(err.to_string().contains("don't match"), "{err}");
        assert!(load_cert_pem(b"", key_b.as_bytes()).is_err());
        assert!(load_cert_pem(cert_a.as_bytes(), b"").is_err());
    }

    #[test]
    fn private_files_are_owner_only() {
        let dir = tempfile::TempDir::new().unwrap();
        let sub = dir.path().join("certs").join("a.example.com");
        create_private_dir(&sub).unwrap();
        let path = sub.join("key.pem");
        write_private(&path, b"secret").unwrap();
        write_private(&path, b"secret2").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"secret2");
        assert!(!path.with_extension("tmp").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
            let mode = std::fs::metadata(&sub).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700);
        }
    }
}
