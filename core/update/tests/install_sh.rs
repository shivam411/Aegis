//! install.sh's release signature check (`verify_release_signature`), run
//! with bash and OpenSSL against signatures made with a key pair generated
//! by the test. Runs on Unix; elsewhere only when `AEGIS_TEST_BASH` names a
//! bash (e.g. Git Bash's).

#[path = "support/minisign.rs"]
mod minisign;

use minisign::TestKey;
use std::path::{Path, PathBuf};
use std::process::Command;

const TAG: &str = "v1.4.0";
const SUMS: &str =
    "3f0a8bd4a3f1b5e0c76e9a8f2c2a7d1c3f0a8bd4a3f1b5e0c76e9a8f2c2a7d1c  aegis-linux-amd64.tar.gz\n";

fn bash() -> Option<String> {
    match std::env::var("AEGIS_TEST_BASH") {
        Ok(path) if !path.is_empty() => Some(path),
        _ if cfg!(unix) => Some("bash".into()),
        _ => None,
    }
}

fn install_sh() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../install.sh")
}

fn slashes(path: &Path) -> String {
    path.display().to_string().replace('\\', "/")
}

struct Run {
    ok: bool,
    output: String,
}

/// Sources install.sh and runs `verify_release_signature` on a directory
/// holding `sums` and (if given) `signature`.
fn verify(
    bash: &str,
    sums: &str,
    signature: Option<&str>,
    keys: &[String],
    allow_unsigned: bool,
) -> Run {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(dir.path().join("SHA256SUMS"), sums).unwrap();
    if let Some(signature) = signature {
        std::fs::write(dir.path().join("SHA256SUMS.minisig"), signature).unwrap();
    }
    let script = r#"set -euo pipefail
export AEGIS_INSTALL_SH_LIB=1
source "$1"
dir="$2"; tag="$3"; ALLOW_UNSIGNED="$4"; shift 4
RELEASE_KEYS=("$@")
verify_release_signature "${dir}" "${tag}"
"#;
    let output = Command::new(bash)
        .arg("-c")
        .arg(script)
        .arg("install-test")
        .arg(slashes(&install_sh()))
        .arg(slashes(dir.path()))
        .arg(TAG)
        .arg(if allow_unsigned { "1" } else { "0" })
        .args(keys)
        .output()
        .expect("run bash");
    Run {
        ok: output.status.success(),
        output: format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    }
}

fn openssl_can_verify(bash: &str) -> bool {
    let script = "export AEGIS_INSTALL_SH_LIB=1; source \"$1\"; openssl_can_verify";
    Command::new(bash)
        .args(["-c", script, "probe"])
        .arg(slashes(&install_sh()))
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[test]
fn install_sh_verifies_release_signatures() {
    let Some(bash) = bash() else {
        eprintln!("skipped: no bash (set AEGIS_TEST_BASH)");
        return;
    };
    if !openssl_can_verify(&bash) {
        // Stock Linux has OpenSSL 1.1.1+; CI must really run this.
        assert!(
            !cfg!(target_os = "linux"),
            "OpenSSL can't verify Ed25519/BLAKE2b here"
        );
        eprintln!("skipped: this OpenSSL can't verify Ed25519 signatures");
        return;
    }
    let key = TestKey::generate();
    let other = TestKey::generate();
    let keys = vec![key.public_base64()];
    let comment = format!("aegis-release:{TAG} file:SHA256SUMS");
    let good = key.sign(SUMS.as_bytes(), &comment);

    let run = verify(&bash, SUMS, Some(&good), &keys, false);
    assert!(run.ok, "{}", run.output);
    assert!(run.output.contains(&key.id()), "{}", run.output);

    // Rotation: any listed key.
    let both = vec![other.public_base64(), key.public_base64()];
    let run = verify(&bash, SUMS, Some(&good), &both, false);
    assert!(run.ok, "{}", run.output);

    let cases: Vec<(&str, String, Option<String>, Vec<String>, &str)> = vec![
        (
            "tampered SHA256SUMS",
            SUMS.replace("3f0a", "3f0b"),
            Some(good.clone()),
            keys.clone(),
            "not valid",
        ),
        (
            "wrong key",
            SUMS.into(),
            Some(other.sign(SUMS.as_bytes(), &comment)),
            keys.clone(),
            "does not trust",
        ),
        (
            "forged key ID",
            SUMS.into(),
            Some(other.sign_as(SUMS.as_bytes(), &comment, key.raw_id())),
            keys.clone(),
            "not valid",
        ),
        (
            "another release",
            SUMS.into(),
            Some(key.sign(SUMS.as_bytes(), "aegis-release:v1.3.0")),
            keys.clone(),
            "another release",
        ),
        (
            "edited trusted comment",
            SUMS.into(),
            Some(good.replace(&comment, "aegis-release:v1.4.0 edited")),
            keys.clone(),
            "not valid",
        ),
        (
            "legacy signature",
            SUMS.into(),
            Some(key.sign_legacy(SUMS.as_bytes(), &comment)),
            keys.clone(),
            "pre-hashed",
        ),
        (
            "garbage",
            SUMS.into(),
            Some("untrusted comment: x\nnot base64\n".into()),
            keys.clone(),
            "not a minisign signature",
        ),
        (
            "missing signature",
            SUMS.into(),
            None,
            keys.clone(),
            "not signed",
        ),
        (
            "no release key (placeholder)",
            SUMS.into(),
            Some(good.clone()),
            vec![],
            "no release signing key",
        ),
    ];
    for (name, sums, signature, keys, expected) in &cases {
        let run = verify(&bash, sums, signature.as_deref(), keys, false);
        assert!(!run.ok, "{name}: accepted\n{}", run.output);
        assert!(run.output.contains(expected), "{name}: {}", run.output);
    }

    // --allow-unsigned accepts a missing signature or key, with a warning...
    for (sums, signature, keys) in [
        (SUMS, None, keys.clone()),
        (SUMS, Some(good.as_str()), vec![]),
    ] {
        let run = verify(&bash, sums, signature, &keys, true);
        assert!(run.ok, "{}", run.output);
        assert!(run.output.contains("--allow-unsigned"), "{}", run.output);
    }
    // ...but never a bad signature.
    let run = verify(
        &bash,
        &SUMS.replace("3f0a", "3f0b"),
        Some(&good),
        &keys,
        true,
    );
    assert!(!run.ok, "{}", run.output);
}
