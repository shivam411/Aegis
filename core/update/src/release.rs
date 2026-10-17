//! GitHub release metadata and picking the asset for this machine.

use serde::{Deserialize, Serialize};

/// The release's checksum file, published by `.github/workflows/release.yml`
/// (`sha256sum * > SHA256SUMS` over every archive).
pub const CHECKSUMS_ASSET: &str = "SHA256SUMS";

/// A published release, reduced to what the updater needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Release {
    /// The git tag, e.g. `v0.2.0`.
    pub tag: String,
    pub name: String,
    /// The release page (release notes).
    pub html_url: String,
    pub published_at: Option<String>,
    pub prerelease: bool,
    pub draft: bool,
    pub assets: Vec<Asset>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Asset {
    pub name: String,
    /// `browser_download_url`.
    pub url: String,
    pub size: u64,
}

impl Release {
    pub fn asset(&self, name: &str) -> Option<&Asset> {
        self.assets.iter().find(|a| a.name == name)
    }
}

fn api_error(value: &serde_json::Value) -> anyhow::Error {
    let message = value
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("no tag_name in the response");
    anyhow::anyhow!("GitHub did not return a release: {}", message)
}

/// Parses a GitHub "get a release" response.
pub fn parse_release(body: &str) -> anyhow::Result<Release> {
    let value: serde_json::Value = serde_json::from_str(body)
        .map_err(|e| anyhow::anyhow!("Unexpected response from GitHub: {}", e))?;
    let tag = match value.get("tag_name").and_then(|v| v.as_str()) {
        Some(tag) if !tag.is_empty() => tag.to_string(),
        _ => return Err(api_error(&value)),
    };
    let text = |key: &str| {
        value
            .get(key)
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string()
    };
    let assets = value
        .get("assets")
        .and_then(|v| v.as_array())
        .map(|list| {
            list.iter()
                .filter_map(|a| {
                    Some(Asset {
                        name: a.get("name")?.as_str()?.to_string(),
                        url: a.get("browser_download_url")?.as_str()?.to_string(),
                        size: a.get("size").and_then(|s| s.as_u64()).unwrap_or(0),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(Release {
        name: Some(text("name"))
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| tag.clone()),
        tag,
        html_url: text("html_url"),
        published_at: value
            .get("published_at")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        prerelease: value
            .get("prerelease")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        draft: value
            .get("draft")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        assets,
    })
}

/// Reads `tag_name` from a GitHub "latest release" response.
pub fn tag_from_api(body: &str) -> anyhow::Result<String> {
    parse_release(body).map(|r| r.tag)
}

/// An OS/architecture pair as named in release archives
/// (`aegis-<os>-<arch>.tar.gz`, see release.yml and install.sh).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Target {
    pub os: &'static str,
    pub arch: &'static str,
}

impl Target {
    /// Maps Rust's `std::env::consts` names to the release names.
    pub fn from_consts(os: &str, arch: &str) -> Option<Self> {
        let os = match os {
            "linux" => "linux",
            "macos" => "darwin",
            "windows" => "windows",
            _ => return None,
        };
        let arch = match arch {
            "x86_64" => "amd64",
            "aarch64" => "arm64",
            _ => return None,
        };
        Some(Self { os, arch })
    }

    /// The machine this binary was built for.
    pub fn current() -> Option<Self> {
        Self::from_consts(std::env::consts::OS, std::env::consts::ARCH)
    }

    pub fn archive_name(&self) -> String {
        let ext = if self.os == "windows" {
            "zip"
        } else {
            "tar.gz"
        };
        format!("aegis-{}-{}.{}", self.os, self.arch, ext)
    }

    /// Release archives for this target are tar.gz files the updater can
    /// unpack (Windows releases are zip files; update those by hand).
    pub fn self_update_supported(&self) -> bool {
        self.os != "windows"
    }
}

impl std::fmt::Display for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}-{}", self.os, self.arch)
    }
}

/// The archive for `target` in `release`.
pub fn select_asset<'a>(release: &'a Release, target: &Target) -> Option<&'a Asset> {
    release.asset(&target.archive_name())
}

#[cfg(test)]
mod tests {
    use super::*;

    const RELEASE: &str = r#"{
        "tag_name": "v0.2.0", "name": "Aegis 0.2.0",
        "html_url": "https://github.com/shivam411/Aegis/releases/tag/v0.2.0",
        "published_at": "2026-10-01T00:00:00Z", "prerelease": false, "draft": false,
        "assets": [
          {"name": "aegis-linux-amd64.tar.gz", "size": 10,
           "browser_download_url": "https://github.com/shivam411/Aegis/releases/download/v0.2.0/aegis-linux-amd64.tar.gz"},
          {"name": "aegis-linux-arm64.tar.gz", "size": 11,
           "browser_download_url": "https://github.com/shivam411/Aegis/releases/download/v0.2.0/aegis-linux-arm64.tar.gz"},
          {"name": "aegis-darwin-arm64.tar.gz", "size": 12,
           "browser_download_url": "https://github.com/shivam411/Aegis/releases/download/v0.2.0/aegis-darwin-arm64.tar.gz"},
          {"name": "aegis-windows-amd64.zip", "size": 13,
           "browser_download_url": "https://github.com/shivam411/Aegis/releases/download/v0.2.0/aegis-windows-amd64.zip"},
          {"name": "SHA256SUMS", "size": 300,
           "browser_download_url": "https://github.com/shivam411/Aegis/releases/download/v0.2.0/SHA256SUMS"}
        ]}"#;

    #[test]
    fn parses_a_release() {
        let r = parse_release(RELEASE).unwrap();
        assert_eq!(r.tag, "v0.2.0");
        assert_eq!(r.name, "Aegis 0.2.0");
        assert!(!r.prerelease && !r.draft);
        assert_eq!(r.assets.len(), 5);
        assert!(r.asset(CHECKSUMS_ASSET).is_some());
        assert_eq!(tag_from_api(RELEASE).unwrap(), "v0.2.0");
        let err = tag_from_api(r#"{"message": "Not Found"}"#).unwrap_err();
        assert!(err.to_string().contains("Not Found"), "{}", err);
        assert!(tag_from_api("<html>").is_err());
        // A missing name falls back to the tag.
        assert_eq!(
            parse_release(r#"{"tag_name":"v1.0.0"}"#).unwrap().name,
            "v1.0.0"
        );
    }

    #[test]
    fn selects_the_asset_per_target() {
        let r = parse_release(RELEASE).unwrap();
        let pick = |os, arch| {
            Target::from_consts(os, arch)
                .and_then(|t| select_asset(&r, &t))
                .map(|a| a.name.clone())
        };
        assert_eq!(
            pick("linux", "x86_64").as_deref(),
            Some("aegis-linux-amd64.tar.gz")
        );
        assert_eq!(
            pick("linux", "aarch64").as_deref(),
            Some("aegis-linux-arm64.tar.gz")
        );
        assert_eq!(
            pick("macos", "aarch64").as_deref(),
            Some("aegis-darwin-arm64.tar.gz")
        );
        assert_eq!(
            pick("windows", "x86_64").as_deref(),
            Some("aegis-windows-amd64.zip")
        );
        // Published for no such machine.
        assert_eq!(pick("macos", "x86_64"), None);
        assert!(Target::from_consts("freebsd", "x86_64").is_none());
        assert!(Target::from_consts("linux", "riscv64").is_none());
        assert!(!Target::from_consts("windows", "x86_64")
            .unwrap()
            .self_update_supported());
        assert!(Target::from_consts("linux", "x86_64")
            .unwrap()
            .self_update_supported());
    }
}
