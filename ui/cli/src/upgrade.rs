//! `aegis upgrade`: compares this build with the latest GitHub release and
//! installs it with the same code as the daemon's self-update
//! (`aegis-update`): the release's SHA256SUMS must be signed by a release key
//! compiled into this binary, and the archive must match it.

use std::path::{Path, PathBuf};

use aegis_update::{helper, parse_version, SignaturePolicy, Source, Target};
pub use aegis_update::{plan, Plan};

/// The repository releases are downloaded from.
pub const REPO: &str = aegis_update::DEFAULT_REPO;

/// Printed (to stderr) whenever `--allow-unsigned` is used.
pub const UNSIGNED_WARNING: &str = "\
WARNING: --allow-unsigned: the release's signature is NOT required. Only its
         SHA-256 checksums are checked, and they come from the same GitHub
         release: anyone able to publish a release could have replaced both.";

/// GitHub (or the loopback test API in `AEGIS_UPDATE_API_URL`).
pub fn source() -> anyhow::Result<Source> {
    Ok(Source::new(
        aegis_update::api_base_override()?,
        REPO,
        &format!("aegis-cli/{}", aegis_update::VERSION),
    ))
}

/// True when the running binary sits where `install.sh --server` puts it,
/// so a per-user upgrade would not replace it.
pub fn is_server_install(exe: &Path) -> bool {
    exe.parent() == Some(Path::new("/usr/local/bin"))
}

/// The daemon binary next to this CLI: `self_update` replaces it, and the
/// CLI/TUI next to it.
pub fn daemon_next_to(exe: &Path) -> PathBuf {
    exe.with_file_name(aegis_update::install::DAEMON_BINARY)
}

/// Downloads, verifies and installs `tag` into the directory of `exe`.
pub async fn install(
    source: &Source,
    exe: &Path,
    tag: &str,
    force: bool,
    allow_unsigned: bool,
) -> anyhow::Result<helper::Installed> {
    let target = Target::current()
        .filter(|t| t.self_update_supported())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Self-update isn't supported on this platform; download {} from https://github.com/{}/releases",
                tag,
                REPO
            )
        })?;
    let current = parse_version(aegis_update::VERSION).ok_or_else(|| {
        anyhow::anyhow!(
            "This build's version '{}' is not comparable",
            aegis_update::VERSION
        )
    })?;
    let daemon = daemon_next_to(exe);
    helper::self_update(helper::SelfUpdate {
        source,
        exe: &daemon,
        target,
        current,
        wanted: Some(tag),
        force,
        verify_runs: true,
        signatures: if allow_unsigned {
            SignaturePolicy::AllowUnsigned
        } else {
            SignaturePolicy::Required
        },
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use aegis_update::tag_from_api;

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

    #[test]
    fn replaces_the_daemon_next_to_the_cli() {
        assert_eq!(
            daemon_next_to(Path::new("/home/me/.local/bin/aegis-cli")),
            Path::new("/home/me/.local/bin/aegis-daemon")
        );
    }
}
