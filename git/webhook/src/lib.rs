//! GitHub webhook support: signature verification and push-event parsing.
//!
//! GitHub signs each delivery with HMAC-SHA256 over the raw body using the
//! webhook's secret, and sends it as `X-Hub-Signature-256: sha256=<hex>`.

use hmac::{Hmac, Mac};
use serde::Deserialize;
use sha2::Sha256;

/// Verifies `X-Hub-Signature-256` for `body`. Comparison is constant-time.
pub fn verify_github_signature(secret: &str, body: &[u8], header: &str) -> bool {
    let Some(hex_sig) = header.strip_prefix("sha256=") else {
        return false;
    };
    let Some(signature) = decode_hex(hex_sig) else {
        return false;
    };
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret.as_bytes()) else {
        return false;
    };
    mac.update(body);
    mac.verify_slice(&signature).is_ok()
}

/// Computes the header value GitHub would send (useful for tests and tools).
pub fn sign_github_payload(secret: &str, body: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("any key length");
    mac.update(body);
    let bytes = mac.finalize().into_bytes();
    let hex: String = bytes.iter().map(|b| format!("{:02x}", b)).collect();
    format!("sha256={}", hex)
}

fn decode_hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

/// The parts of a GitHub `push` event Aegis uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushEvent {
    /// Branch name, e.g. `main` (from `refs/heads/main`).
    pub branch: Option<String>,
    /// Commit the branch now points to.
    pub after: String,
    pub deleted: bool,
    pub pusher: Option<String>,
}

#[derive(Deserialize)]
struct RawPush {
    #[serde(rename = "ref")]
    git_ref: String,
    after: String,
    #[serde(default)]
    deleted: bool,
    pusher: Option<RawPusher>,
}

#[derive(Deserialize)]
struct RawPusher {
    name: Option<String>,
}

/// Parses a `push` event body.
pub fn parse_push(body: &[u8]) -> Result<PushEvent, anyhow::Error> {
    let raw: RawPush = serde_json::from_slice(body)?;
    let is_hex = raw.after.len() == 40 && raw.after.chars().all(|c| c.is_ascii_hexdigit());
    if !is_hex {
        anyhow::bail!("push event has an invalid 'after' commit");
    }
    Ok(PushEvent {
        branch: raw.git_ref.strip_prefix("refs/heads/").map(str::to_string),
        after: raw.after,
        deleted: raw.deleted,
        pusher: raw.pusher.and_then(|p| p.name),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_signature_round_trip_and_rejections() {
        let body = br#"{"zen":"Keep it logically awesome."}"#;
        let header = sign_github_payload("s3cret", body);
        assert!(header.starts_with("sha256="));
        assert!(verify_github_signature("s3cret", body, &header));
        assert!(!verify_github_signature("other", body, &header));
        assert!(!verify_github_signature("s3cret", b"tampered", &header));
        assert!(!verify_github_signature("s3cret", body, "sha1=abcd"));
        assert!(!verify_github_signature("s3cret", body, "sha256=zz"));
        assert!(!verify_github_signature("s3cret", body, ""));
    }

    #[test]
    fn test_known_vector() {
        // Example from GitHub's "Validating webhook deliveries" docs.
        assert_eq!(
            sign_github_payload("It's a Secret to Everybody", b"Hello, World!"),
            "sha256=757107ea0eb2509fc211221cce984b8a37570b6d7586c22c46f4379c8b043e17"
        );
    }

    #[test]
    fn test_parse_push() {
        let body = br#"{"ref":"refs/heads/main","after":"0123456789abcdef0123456789abcdef01234567","deleted":false,"pusher":{"name":"octocat"},"repository":{"clone_url":"x"}}"#;
        let push = parse_push(body).unwrap();
        assert_eq!(push.branch.as_deref(), Some("main"));
        assert_eq!(push.pusher.as_deref(), Some("octocat"));
        assert!(!push.deleted);

        let tag = br#"{"ref":"refs/tags/v1","after":"0123456789abcdef0123456789abcdef01234567"}"#;
        assert_eq!(parse_push(tag).unwrap().branch, None);
        assert!(parse_push(br#"{"ref":"refs/heads/x","after":"--upload-pack=evil"}"#).is_err());
        assert!(parse_push(b"not json").is_err());
    }
}
