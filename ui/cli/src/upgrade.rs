//! `aegis upgrade`: compares this build with the latest GitHub release and
//! re-runs install.sh to update. Uses the system `curl`, like install.sh.
//! Version handling is shared with the daemon's updater (`aegis-update`).

use std::path::Path;
use std::process::Command;

pub use aegis_update::{plan, tag_from_api, Plan};

/// The repository install.sh downloads releases from.
pub const REPO: &str = aegis_update::DEFAULT_REPO;
/// Where install.sh is published (see the header of install.sh).
pub const INSTALL_SCRIPT_URL: &str = "https://shivam411.github.io/Aegis/install.sh";

pub fn latest_release_api() -> String {
    format!("https://api.github.com/repos/{}/releases/latest", REPO)
}

fn curl(args: &[&str]) -> anyhow::Result<Vec<u8>> {
    let output = Command::new("curl").args(args).output().map_err(|e| {
        anyhow::anyhow!(
            "Could not run curl ({}). Install curl, or download a release from https://github.com/{}/releases",
            e,
            REPO
        )
    })?;
    if !output.status.success() {
        anyhow::bail!(
            "curl failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output.stdout)
}

/// The tag of the latest published release, e.g. "v0.1.7".
pub fn fetch_latest_tag() -> anyhow::Result<String> {
    let body = curl(&[
        "-fsSL",
        "--max-time",
        "20",
        "-H",
        "Accept: application/vnd.github+json",
        &latest_release_api(),
    ])?;
    tag_from_api(&String::from_utf8_lossy(&body))
}

/// True when the running binary sits where `install.sh --server` puts it,
/// so a per-user install would not replace it.
pub fn is_server_install(exe: &Path) -> bool {
    exe.parent() == Some(Path::new("/usr/local/bin"))
}

/// Downloads install.sh and runs it with bash (it uses bash features).
pub fn run_installer() -> anyhow::Result<()> {
    let script = std::env::temp_dir().join(format!("aegis-install-{}.sh", std::process::id()));
    let path = script.to_string_lossy().to_string();
    curl(&["-fsSL", "--max-time", "60", "-o", &path, INSTALL_SCRIPT_URL])?;
    let status = Command::new("bash").arg(&script).status();
    let _ = std::fs::remove_file(&script);
    let status = status.map_err(|e| anyhow::anyhow!("Could not run bash: {}", e))?;
    if !status.success() {
        anyhow::bail!("install.sh failed ({})", status);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use aegis_update::parse_version;

    #[test]
    fn compares_versions() {
        let v = |s: &str| parse_version(s).unwrap();
        assert_eq!(v("v0.1.7").core, (0, 1, 7));
        assert!(v("0.2.0") > v("0.1.10"));
        assert!(v("0.1.10") > v("0.1.9"));
        assert!(v("1.0.0") > v("1.0.0-rc.1"));
        assert!(v("1.0.0-rc.2") > v("1.0.0-rc.1"));
        assert!(v("1.0.0-rc.10") > v("1.0.0-rc.9"));
        assert!(v("1.0.0-beta") > v("1.0.0-alpha"));
        assert!(v("1.0.0-alpha.1") > v("1.0.0-alpha"));
        assert!(v("1.0.0-alpha") > v("1.0.0-1"));
        assert_eq!(v("1.2.3+build.5"), v("v1.2.3"));
        for bad in ["", "1.2", "1.2.3.4", "x.y.z", "latest"] {
            assert!(parse_version(bad).is_none(), "{}", bad);
        }
    }

    #[test]
    fn plans_upgrades() {
        assert_eq!(plan("v0.1.7", "0.1.0").unwrap(), Plan::Upgrade);
        assert_eq!(plan("v0.1.7", "0.1.7").unwrap(), Plan::UpToDate);
        assert_eq!(plan("v0.1.7", "0.2.0-dev").unwrap(), Plan::Ahead);
        assert!(plan("nightly", "0.1.0").is_err());
    }

    #[test]
    fn reads_the_release_tag() {
        assert_eq!(
            tag_from_api(r#"{"tag_name": "v0.1.7", "name": "x"}"#).unwrap(),
            "v0.1.7"
        );
        let err = tag_from_api(r#"{"message": "Not Found"}"#).unwrap_err();
        assert!(err.to_string().contains("Not Found"), "{}", err);
        assert!(tag_from_api("<html>").is_err());
    }

    #[test]
    fn detects_server_installs() {
        assert!(is_server_install(Path::new("/usr/local/bin/aegis-cli")));
        assert!(!is_server_install(Path::new(
            "/home/me/.local/bin/aegis-cli"
        )));
    }
}
