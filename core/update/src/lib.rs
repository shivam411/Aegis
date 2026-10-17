//! Aegis self-update, shared by the daemon (update checks, dashboard
//! upgrades, `aegis-daemon self-update`) and the CLI (`aegis upgrade`).
//!
//! Trust model: release metadata and archives come from GitHub over TLS
//! (certificates checked against the bundled Mozilla roots), and every
//! archive must match the SHA-256 listed in the same release's `SHA256SUMS`
//! before anything is unpacked. See docs/security.md.

pub mod archive;
pub mod checksum;
pub mod helper;
pub mod http;
pub mod install;
pub mod manager;
pub mod release;
pub mod source;
mod state;
pub mod version;

pub use install::InstallMode;
pub use manager::{Phase, UpdateError, UpdateHooks, UpdateManager, UpdateSettings, UpdateStatus};
pub use release::{parse_release, select_asset, tag_from_api, Asset, Release, Target};
pub use source::Source;
pub use state::AppliedUpdate;
pub use version::{is_major_jump, parse_version, plan, Plan, Version};

/// This build's version: `AEGIS_VERSION` at build time (release.yml sets it
/// from the git tag), else the crate version.
pub const VERSION: &str = match option_env!("AEGIS_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};

/// The repository releases are published in.
pub const DEFAULT_REPO: &str = "shivam411/Aegis";
pub const DEFAULT_API_BASE: &str = "https://api.github.com";

/// Points update checks at a local fake GitHub API (tests only). Only
/// `http(s)://` URLs on a loopback address are accepted.
pub const API_URL_ENV: &str = "AEGIS_UPDATE_API_URL";

/// Validates an API base override: loopback only, no path.
pub fn validate_api_override(url: &str) -> anyhow::Result<String> {
    let url = url.trim().trim_end_matches('/');
    let uri: hyper::Uri = url
        .parse()
        .map_err(|_| anyhow::anyhow!("{} is not a URL", API_URL_ENV))?;
    let loopback = uri.host().is_some_and(http::is_loopback_host);
    let plain = matches!(uri.path(), "" | "/") && uri.query().is_none();
    if !matches!(uri.scheme_str(), Some("http" | "https")) || !loopback || !plain {
        anyhow::bail!(
            "{} must be http://127.0.0.1:PORT or similar (a loopback address; it exists for tests)",
            API_URL_ENV
        );
    }
    Ok(url.to_string())
}

/// The API base override from the environment, if set (and valid).
pub fn api_base_override() -> anyhow::Result<Option<String>> {
    match std::env::var(API_URL_ENV) {
        Ok(v) if !v.trim().is_empty() => validate_api_override(&v).map(Some),
        _ => Ok(None),
    }
}

/// `owner/name` with GitHub's allowed characters.
pub fn valid_repo(repo: &str) -> bool {
    let ok = |s: &str| {
        !s.is_empty()
            && s.len() <= 100
            && s != "."
            && s != ".."
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    matches!(repo.split_once('/'), Some((owner, name)) if ok(owner) && ok(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_override_is_loopback_only() {
        assert_eq!(
            validate_api_override("http://127.0.0.1:8080/").unwrap(),
            "http://127.0.0.1:8080"
        );
        assert!(validate_api_override("http://localhost:1").is_ok());
        assert!(validate_api_override("http://[::1]:1").is_ok());
        for bad in [
            "https://api.github.com",
            "http://10.0.0.1:80",
            "http://127.0.0.1:80/repos",
            "file:///etc/passwd",
            "127.0.0.1:80",
            "http://127.0.0.1.evil.com",
        ] {
            assert!(validate_api_override(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn repo_names() {
        assert!(valid_repo("shivam411/Aegis"));
        assert!(valid_repo("my-org/aegis.fork_2"));
        for bad in ["", "Aegis", "a/b/c", "../x", "a/..", "a /b", "a/b?x"] {
            assert!(!valid_repo(bad), "{bad}");
        }
    }

    #[test]
    fn version_is_parseable() {
        assert!(parse_version(VERSION).is_some(), "{VERSION}");
    }
}
