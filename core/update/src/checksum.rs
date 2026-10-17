//! `SHA256SUMS` parsing and verification.

use sha2::{Digest, Sha256};
use std::collections::HashMap;

pub type Sha256Digest = [u8; 32];

pub fn sha256(bytes: &[u8]) -> Sha256Digest {
    Sha256::digest(bytes).into()
}

pub fn to_hex(digest: &Sha256Digest) -> String {
    digest.iter().map(|b| format!("{:02x}", b)).collect()
}

fn from_hex(s: &str) -> Option<Sha256Digest> {
    if s.len() != 64 || !s.is_ascii() {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
}

/// Parses `sha256sum` output: `<64 hex>  <name>` (text mode) or
/// `<64 hex> *<name>` (binary mode), one file per line. Any malformed line
/// rejects the whole file, as does a name listed twice with different sums.
pub fn parse_sums(text: &str) -> anyhow::Result<HashMap<String, Sha256Digest>> {
    let mut sums = HashMap::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.trim_end_matches('\r');
        if line.trim().is_empty() {
            continue;
        }
        let bad = || anyhow::anyhow!("SHA256SUMS line {} is not '<sha256>  <file>'", n + 1);
        let (hex, rest) = line.split_once(' ').ok_or_else(bad)?;
        let digest = from_hex(&hex.to_ascii_lowercase()).ok_or_else(bad)?;
        let name = rest
            .strip_prefix(' ')
            .or_else(|| rest.strip_prefix('*'))
            .ok_or_else(bad)?;
        // Names are bare file names; "./x" is how `sha256sum ./x` prints them.
        let name = name.strip_prefix("./").unwrap_or(name);
        if name.is_empty() || name.contains('/') || name.contains('\\') {
            return Err(bad());
        }
        if let Some(previous) = sums.insert(name.to_string(), digest) {
            if previous != digest {
                anyhow::bail!("SHA256SUMS lists {} twice with different sums", name);
            }
        }
    }
    if sums.is_empty() {
        anyhow::bail!("SHA256SUMS is empty");
    }
    Ok(sums)
}

/// Checks `bytes` against the sum listed for `name`.
pub fn verify(
    bytes: &[u8],
    name: &str,
    sums: &HashMap<String, Sha256Digest>,
) -> anyhow::Result<()> {
    let expected = sums
        .get(name)
        .ok_or_else(|| anyhow::anyhow!("SHA256SUMS has no entry for {}", name))?;
    let actual = sha256(bytes);
    if &actual != expected {
        anyhow::bail!(
            "Checksum mismatch for {}: expected {}, got {}. The download is corrupt or was tampered with; nothing was installed.",
            name,
            to_hex(expected),
            to_hex(&actual)
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sha256sum_output() {
        let a = to_hex(&sha256(b"a"));
        let b = to_hex(&sha256(b"b"));
        let text = format!("{a}  aegis-linux-amd64.tar.gz\r\n\n{b} *./aegis-darwin-arm64.tar.gz\n");
        let sums = parse_sums(&text).unwrap();
        assert_eq!(sums.len(), 2);
        assert_eq!(sums["aegis-linux-amd64.tar.gz"], sha256(b"a"));
        assert_eq!(sums["aegis-darwin-arm64.tar.gz"], sha256(b"b"));
        // Upper-case hex is fine too.
        let upper = format!("{}  x", a.to_uppercase());
        assert_eq!(parse_sums(&upper).unwrap()["x"], sha256(b"a"));
    }

    #[test]
    fn rejects_malformed_files() {
        let a = to_hex(&sha256(b"a"));
        for bad in [
            String::new(),
            "not a checksum".to_string(),
            format!("{}  x", &a[..63]),
            format!("{}z  x", &a[..63]),
            format!("{a}x"),
            format!("{a}  "),
            format!("{a}  ../etc/passwd"),
            format!("{a}  x\n{}  x", to_hex(&sha256(b"b"))),
        ] {
            assert!(parse_sums(&bad).is_err(), "{bad:?}");
        }
        // The same entry twice is harmless.
        assert!(parse_sums(&format!("{a}  x\n{a}  x")).is_ok());
    }

    #[test]
    fn verification_rejects_tampered_files() {
        let sums = parse_sums(&format!("{}  aegis.tar.gz", to_hex(&sha256(b"good")))).unwrap();
        verify(b"good", "aegis.tar.gz", &sums).unwrap();
        let err = verify(b"evil", "aegis.tar.gz", &sums).unwrap_err();
        assert!(err.to_string().contains("Checksum mismatch"), "{err}");
        let err = verify(b"good", "other.tar.gz", &sums).unwrap_err();
        assert!(err.to_string().contains("no entry"), "{err}");
    }
}
