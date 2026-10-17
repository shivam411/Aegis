//! Release signatures.
//!
//! The release workflow signs each release's `SHA256SUMS` with the
//! maintainers' minisign key (Ed25519, pre-hashed with BLAKE2b-512) and
//! publishes `SHA256SUMS.minisig`. Its trusted comment names the release
//! (`aegis-release:<tag>`), so a signed checksum file can't be replayed on a
//! different release. The public keys this build trusts are compiled in from
//! `core/update/keys/release.pub`; several may be listed for key rotation.
//! When that file lists no key (the placeholder), nothing verifies and every
//! install path refuses unless the operator explicitly allows unsigned
//! releases on the command line. See docs/releasing.md and docs/security.md.

use base64::Engine;
use minisign_verify::{PublicKey, Signature};
use std::sync::OnceLock;

/// The signature published next to the release's `SHA256SUMS`.
pub const SIGNATURE_ASSET: &str = "SHA256SUMS.minisig";

/// The trusted comment names the release it was made for:
/// `aegis-release:v1.2.3` (other words may follow).
pub const RELEASE_COMMENT_PREFIX: &str = "aegis-release:";

/// The committed public-key file, compiled in.
pub const RELEASE_KEYS_FILE: &str = include_str!("../keys/release.pub");

/// What went wrong verifying a release's signature. Reported as stage
/// `signature` in `UpdateFailed` events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignatureError {
    /// This build trusts no key (the placeholder key file).
    NoTrustedKey,
    /// The release has no `SHA256SUMS.minisig`.
    Missing { tag: String },
    /// The signature file can't be parsed.
    Malformed(String),
    /// Signed by a key this build doesn't trust.
    UnknownKey {
        key_id: String,
        trusted: Vec<String>,
    },
    /// The signature doesn't match `SHA256SUMS` (tampered or corrupt).
    Invalid { key_id: String },
    /// A valid signature, but for another release.
    WrongRelease { tag: String, comment: String },
}

impl std::fmt::Display for SignatureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoTrustedKey => write!(
                f,
                "This build of Aegis has no release signing key (core/update/keys/release.pub is a placeholder), so it can't check that a release is genuine; refusing to install it. Install the release by hand, or on the server run `aegis-daemon self-update --allow-unsigned` (checks SHA-256 only). See docs/upgrading.md."
            ),
            Self::Missing { tag } => write!(
                f,
                "Release {} has no {} (it is not signed); refusing to install it. To install an unsigned release anyway, run `aegis-daemon self-update --version {} --allow-unsigned` on the server.",
                tag, SIGNATURE_ASSET, tag
            ),
            Self::Malformed(e) => write!(f, "{} is not a minisign signature: {}", SIGNATURE_ASSET, e),
            Self::UnknownKey { key_id, trusted } => write!(
                f,
                "SHA256SUMS is signed by key {}, which this build does not trust (trusted: {}); refusing to install it.",
                key_id,
                trusted.join(", ")
            ),
            Self::Invalid { key_id } => write!(
                f,
                "The signature on SHA256SUMS (key {}) is not valid: the file was modified or corrupted; nothing was installed.",
                key_id
            ),
            Self::WrongRelease { tag, comment } => write!(
                f,
                "The signature on SHA256SUMS was made for another release ('{}', expected {}{}); refusing to install it.",
                comment, RELEASE_COMMENT_PREFIX, tag
            ),
        }
    }
}

impl std::error::Error for SignatureError {}

/// True when `error` (or its cause) is a signature failure.
pub fn is_signature_error(error: &anyhow::Error) -> bool {
    error.downcast_ref::<SignatureError>().is_some()
}

/// Whether a release without a valid signature may be installed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignaturePolicy {
    /// The default everywhere: a valid signature by a trusted key.
    Required,
    /// `--allow-unsigned` (command line only): a missing signature, or no
    /// trusted key in this build, is accepted with a warning. A signature
    /// that is present but invalid is still refused.
    AllowUnsigned,
}

/// minisign shows key IDs as the little-endian u64, in upper-case hex.
fn key_id_hex(bytes: &[u8]) -> String {
    let mut id = [0u8; 8];
    id.copy_from_slice(&bytes[..8]);
    format!("{:016X}", u64::from_le_bytes(id))
}

#[derive(Clone, Debug)]
struct TrustedKey {
    id: String,
    key: PublicKey,
}

/// The public keys release signatures are checked against.
#[derive(Clone, Debug, Default)]
pub struct Keyring {
    keys: Vec<TrustedKey>,
}

impl Keyring {
    /// Parses a key file: one base64 minisign public key per line; blank
    /// lines, `#` comments and `untrusted comment:` lines are skipped.
    pub fn parse(text: &str) -> anyhow::Result<Self> {
        let mut keys: Vec<TrustedKey> = Vec::new();
        for (n, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line.starts_with("untrusted comment:") {
                continue;
            }
            let bad = || anyhow::anyhow!("Line {} is not a minisign public key", n + 1);
            let raw = base64::engine::general_purpose::STANDARD
                .decode(line)
                .map_err(|_| bad())?;
            if raw.len() != 42 || &raw[..2] != b"Ed" {
                return Err(bad());
            }
            let key = PublicKey::from_base64(line).map_err(|_| bad())?;
            let id = key_id_hex(&raw[2..10]);
            if keys.iter().any(|k| k.id == id) {
                anyhow::bail!("Key {} is listed twice", id);
            }
            keys.push(TrustedKey { id, key });
        }
        Ok(Self { keys })
    }

    /// The keys compiled into this build. An unreadable key file (caught by
    /// the tests) counts as no key, so verification fails closed.
    pub fn release() -> &'static Keyring {
        static KEYS: OnceLock<Keyring> = OnceLock::new();
        KEYS.get_or_init(|| {
            Keyring::parse(RELEASE_KEYS_FILE).unwrap_or_else(|e| {
                tracing::error!(error = %e, "The compiled-in release key file is invalid; no release can be verified");
                Keyring::default()
            })
        })
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// The trusted key IDs, as minisign prints them.
    pub fn ids(&self) -> Vec<String> {
        self.keys.iter().map(|k| k.id.clone()).collect()
    }

    /// Checks `signature` (the text of `SHA256SUMS.minisig`) over `sums`
    /// (the bytes of `SHA256SUMS`) for release `tag`. Returns the ID of the
    /// key that signed it.
    pub fn verify(
        &self,
        sums: &[u8],
        signature: &str,
        tag: &str,
    ) -> Result<String, SignatureError> {
        if self.keys.is_empty() {
            return Err(SignatureError::NoTrustedKey);
        }
        let parsed =
            Signature::decode(signature).map_err(|e| SignatureError::Malformed(e.to_string()))?;
        // The key ID sits in bytes 2..10 of the signature line.
        let key_id = signature
            .lines()
            .nth(1)
            .and_then(|l| {
                base64::engine::general_purpose::STANDARD
                    .decode(l.trim())
                    .ok()
            })
            .filter(|raw| raw.len() == 74)
            .map(|raw| key_id_hex(&raw[2..10]))
            .ok_or_else(|| SignatureError::Malformed("no key ID".into()))?;
        let key = self.keys.iter().find(|k| k.id == key_id).ok_or_else(|| {
            SignatureError::UnknownKey {
                key_id: key_id.clone(),
                trusted: self.ids(),
            }
        })?;
        // Pre-hashed signatures only (minisign's default since 0.8).
        key.key
            .verify(sums, &parsed, false)
            .map_err(|_| SignatureError::Invalid {
                key_id: key_id.clone(),
            })?;
        let comment = parsed.trusted_comment();
        let wanted = format!("{}{}", RELEASE_COMMENT_PREFIX, tag);
        if !comment.split_whitespace().any(|w| w == wanted) {
            return Err(SignatureError::WrongRelease {
                tag: tag.to_string(),
                comment: comment.to_string(),
            });
        }
        Ok(key_id)
    }
}

#[cfg(test)]
#[path = "../tests/support/minisign.rs"]
mod test_key;

#[cfg(test)]
mod tests {
    use super::test_key::TestKey;
    use super::*;

    const SUMS: &[u8] = b"0123  aegis-linux-amd64.tar.gz\n";

    fn ring_of(keys: &[&TestKey]) -> Keyring {
        let text: String = keys
            .iter()
            .map(|k| {
                format!(
                    "untrusted comment: minisign public key {}\n{}\n",
                    k.id(),
                    k.public_base64()
                )
            })
            .collect();
        Keyring::parse(&text).unwrap()
    }

    #[test]
    fn accepts_a_valid_signature() {
        let key = TestKey::generate();
        let ring = ring_of(&[&key]);
        let sig = key.sign(SUMS, "aegis-release:v1.2.0 file:SHA256SUMS");
        assert_eq!(ring.verify(SUMS, &sig, "v1.2.0").unwrap(), key.id());
        assert_eq!(ring.ids(), vec![key.id()]);
    }

    #[test]
    fn rejects_tampering_wrong_keys_and_other_releases() {
        let key = TestKey::generate();
        let other = TestKey::generate();
        let ring = ring_of(&[&key]);
        let sig = key.sign(SUMS, "aegis-release:v1.2.0");
        // Tampered SHA256SUMS.
        let err = ring
            .verify(b"0124  aegis-linux-amd64.tar.gz\n", &sig, "v1.2.0")
            .unwrap_err();
        assert!(matches!(err, SignatureError::Invalid { .. }), "{err}");
        // Signed by another key.
        let err = ring
            .verify(SUMS, &other.sign(SUMS, "aegis-release:v1.2.0"), "v1.2.0")
            .unwrap_err();
        assert!(
            matches!(&err, SignatureError::UnknownKey { key_id, .. } if *key_id == other.id()),
            "{err}"
        );
        // Another key that claims our key ID.
        let forged = other.sign_as(SUMS, "aegis-release:v1.2.0", key.raw_id());
        let err = ring.verify(SUMS, &forged, "v1.2.0").unwrap_err();
        assert!(matches!(err, SignatureError::Invalid { .. }), "{err}");
        // A genuine signature for another release.
        let err = ring.verify(SUMS, &sig, "v1.3.0").unwrap_err();
        assert!(matches!(err, SignatureError::WrongRelease { .. }), "{err}");
        let sig = key.sign(SUMS, "aegis-release:v1.2.0-rc.1");
        assert!(ring.verify(SUMS, &sig, "v1.2.0").is_err());
        // A tampered trusted comment.
        let sig = key.sign(SUMS, "aegis-release:v1.2.0");
        let edited = sig.replace("aegis-release:v1.2.0", "aegis-release:v1.3.0");
        let err = ring.verify(SUMS, &edited, "v1.3.0").unwrap_err();
        assert!(matches!(err, SignatureError::Invalid { .. }), "{err}");
        // Garbage, and a missing file's empty text.
        for bad in ["", "not a signature", "untrusted comment: x\nAAAA\n"] {
            let err = ring.verify(SUMS, bad, "v1.2.0").unwrap_err();
            assert!(
                matches!(err, SignatureError::Malformed(_)),
                "{bad:?}: {err}"
            );
        }
    }

    #[test]
    fn legacy_signatures_are_refused() {
        let key = TestKey::generate();
        let ring = ring_of(&[&key]);
        let sig = key.sign_legacy(SUMS, "aegis-release:v1.2.0");
        assert!(ring.verify(SUMS, &sig, "v1.2.0").is_err());
    }

    #[test]
    fn any_trusted_key_verifies_for_rotation() {
        let old = TestKey::generate();
        let new = TestKey::generate();
        let ring = ring_of(&[&old, &new]);
        for key in [&old, &new] {
            let sig = key.sign(SUMS, "aegis-release:v2.0.0");
            assert_eq!(ring.verify(SUMS, &sig, "v2.0.0").unwrap(), key.id());
        }
        // After the old key is retired, its signatures no longer count.
        let ring = ring_of(&[&new]);
        let sig = old.sign(SUMS, "aegis-release:v2.0.0");
        assert!(ring.verify(SUMS, &sig, "v2.0.0").is_err());
    }

    #[test]
    fn an_empty_keyring_fails_closed() {
        let key = TestKey::generate();
        let sig = key.sign(SUMS, "aegis-release:v1.2.0");
        let ring = Keyring::parse("# placeholder\n\n").unwrap();
        assert!(ring.is_empty());
        let err = ring.verify(SUMS, &sig, "v1.2.0").unwrap_err();
        assert_eq!(err, SignatureError::NoTrustedKey);
        assert!(err.to_string().contains("--allow-unsigned"), "{err}");
    }

    #[test]
    fn key_files_are_validated() {
        let key = TestKey::generate();
        assert!(Keyring::parse("not base64!").is_err());
        assert!(Keyring::parse("AAAA").is_err());
        let twice = format!("{}\n{}\n", key.public_base64(), key.public_base64());
        assert!(Keyring::parse(&twice).is_err());
    }

    #[test]
    fn the_committed_key_file_parses() {
        // Either the placeholder (no key) or valid keys; never a broken file
        // that would silently disable updates.
        Keyring::parse(RELEASE_KEYS_FILE).unwrap();
    }

    /// install.sh verifies with the same keys before Aegis is installed.
    #[test]
    fn install_sh_lists_the_same_keys() {
        let script = include_str!("../../../install.sh");
        let start = script
            .find("RELEASE_KEYS=(")
            .expect("install.sh defines RELEASE_KEYS");
        let body = &script[start + "RELEASE_KEYS=(".len()..];
        let body = &body[..body.find(')').expect("RELEASE_KEYS=( ... )")];
        let listed: Vec<String> = body
            .lines()
            .map(|l| l.split('#').next().unwrap_or("").trim())
            .flat_map(|l| l.split_whitespace())
            .map(|w| w.trim_matches('"').trim_matches('\'').to_string())
            .filter(|w| !w.is_empty())
            .collect();
        let committed: Vec<String> = RELEASE_KEYS_FILE
            .lines()
            .map(str::trim)
            .filter(|l| {
                !l.is_empty() && !l.starts_with('#') && !l.starts_with("untrusted comment:")
            })
            .map(str::to_string)
            .collect();
        assert_eq!(
            listed, committed,
            "install.sh RELEASE_KEYS != core/update/keys/release.pub"
        );
    }
}
