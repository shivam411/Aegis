//! `aegis-daemon self-update`, the updater's settings, and restarting into a
//! newly installed binary.

use crate::service::DAEMON_VERSION;
use aegis_config::Config;
use aegis_update::{helper, parse_version, SignaturePolicy, Source, Target, UpdateSettings};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Exit status that tells systemd to start the daemon again
/// (`RestartForceExitStatus=75` in install.sh's unit; `Restart=on-failure`
/// restarts on it too). EX_TEMPFAIL.
pub const RESTART_EXIT_CODE: i32 = 75;

fn under_systemd() -> bool {
    std::env::var_os("INVOCATION_ID").is_some()
}

fn exe_path() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("aegis-daemon"))
}

/// The command an administrator runs when the daemon can't replace its own
/// binary.
fn manual_command(exe: &Path, config_path: &str) -> String {
    let config = std::fs::canonicalize(config_path)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| config_path.to_string());
    let restart = if under_systemd() {
        " --restart-unit aegis.service"
    } else {
        ""
    };
    format!(
        "sudo env AEGIS_CONFIG={} {} self-update{}",
        config,
        exe.display(),
        restart
    )
}

pub fn settings(config: &Config, config_path: &str) -> UpdateSettings {
    let exe = exe_path();
    let state_dir = config.daemon.resolved_data_dir().join("update");
    let mut settings = UpdateSettings::new(DAEMON_VERSION, exe.clone(), state_dir);
    settings.check = config.update.check;
    settings.interval = Duration::from_secs(config.update.interval_hours * 3600);
    settings.repo = config.update.repo.clone();
    // Validated at startup (validate_config).
    settings.api_base = aegis_update::api_base_override().ok().flatten();
    if settings.api_base.is_some() {
        tracing::warn!(
            "{} is set: update checks use a local test server, not GitHub",
            aegis_update::API_URL_ENV
        );
        // Tests point at a local fake right away.
        settings.initial_delay = Duration::from_secs(1);
    }
    settings.manual_command = manual_command(&exe, config_path);
    settings
}

/// Starts the newly installed binary in place of this process. Under
/// systemd: exit with [`RESTART_EXIT_CODE`] and let systemd start it (a fresh
/// cgroup and environment). Otherwise: re-exec the same path with the same
/// arguments (Unix), or start it and exit (Windows).
pub fn restart_self() -> ! {
    if under_systemd() {
        eprintln!("aegis-daemon: exiting for systemd to start the updated binary");
        std::process::exit(RESTART_EXIT_CODE);
    }
    let exe = exe_path();
    let args: Vec<String> = std::env::args().skip(1).collect();
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let err = std::process::Command::new(&exe).args(&args).exec();
        eprintln!(
            "aegis-daemon: could not start {} ({}); start it again by hand",
            exe.display(),
            err
        );
        std::process::exit(1);
    }
    #[cfg(not(unix))]
    {
        match std::process::Command::new(&exe).args(&args).spawn() {
            Ok(_) => std::process::exit(0),
            Err(e) => {
                eprintln!(
                    "aegis-daemon: could not start {} ({}); start it again by hand",
                    exe.display(),
                    e
                );
                std::process::exit(1);
            }
        }
    }
}

fn valid_unit(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '@' | '.' | '_' | '-'))
}

fn restart_unit(unit: &str) -> Result<(), anyhow::Error> {
    let status = std::process::Command::new("systemctl")
        .args(["restart", "--no-block", unit])
        .status()
        .map_err(|e| anyhow::anyhow!("Cannot run systemctl: {}", e))?;
    if !status.success() {
        anyhow::bail!("systemctl restart {} failed ({})", unit, status);
    }
    Ok(())
}

/// Printed (to stderr) whenever `--allow-unsigned` is used.
const UNSIGNED_WARNING: &str = "\
WARNING: --allow-unsigned: the release's signature is NOT required.
         Only its SHA-256 checksums are checked, and they come from the same
         GitHub release: anyone able to publish a release could have replaced
         both. Use this only for a release you have verified another way.
         The install is recorded in the event log (UpdateApplied, allow_unsigned).";

/// The pre-update backup, taken once the new release is downloaded and
/// verified, just before the binaries are replaced. The same archive as
/// the dashboard's "Upgrade now" takes (without releases).
struct PreUpdateBackup<'a> {
    config: &'a Config,
    taken: std::sync::Mutex<Option<String>>,
}

#[async_trait::async_trait]
impl helper::BeforeSwap for PreUpdateBackup<'_> {
    async fn before_swap(&self, tag: &str) -> anyhow::Result<()> {
        match crate::pre_update_backup(self.config).await {
            Ok(Some(name)) => {
                println!("Backup before installing {}: {}", tag, name);
                *self.taken.lock().unwrap() = Some(name);
                Ok(())
            }
            Ok(None) => {
                println!("No database yet; nothing to back up.");
                Ok(())
            }
            Err(e) => Err(anyhow::anyhow!(
                "The pre-update backup failed, so nothing was installed: {:#}. Fix that (e.g. free disk space), or run again with --no-backup to update without a backup",
                e
            )),
        }
    }
}

/// `aegis-daemon self-update [--check] [--version TAG] [--force]
/// [--restart-unit UNIT] [--from-request] [--allow-unsigned] [--no-backup]`.
pub async fn command(config: &Config, flags: &[&str]) -> Result<(), anyhow::Error> {
    let mut check = false;
    let mut force = false;
    let mut from_request = false;
    let mut allow_unsigned = false;
    let mut no_backup = false;
    let mut wanted = None;
    let mut unit = None;
    let mut it = flags.iter();
    while let Some(flag) = it.next() {
        match *flag {
            "--check" => check = true,
            "--force" => force = true,
            "--from-request" => from_request = true,
            "--allow-unsigned" => allow_unsigned = true,
            "--no-backup" => no_backup = true,
            "--version" => {
                let tag = it
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--version needs a tag like v0.2.0"))?;
                if parse_version(tag).is_none() {
                    anyhow::bail!("'{}' is not a version tag like v0.2.0", tag);
                }
                wanted = Some(tag.to_string());
            }
            "--restart-unit" => {
                let name = it
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--restart-unit needs a unit name"))?;
                if !valid_unit(name) {
                    anyhow::bail!("'{}' is not a systemd unit name", name);
                }
                unit = Some(name.to_string());
            }
            other => anyhow::bail!("Unknown flag {}\n\n{}", other, crate::USAGE),
        }
    }
    let source = Source::new(
        aegis_update::api_base_override()?,
        &config.update.repo,
        &format!("aegis-daemon/{}", DAEMON_VERSION),
    );
    let current = parse_version(DAEMON_VERSION).ok_or_else(|| {
        anyhow::anyhow!(
            "This build's version '{}' is not comparable",
            DAEMON_VERSION
        )
    })?;

    if check {
        let latest = source.latest().await?;
        println!("Running:        {}", DAEMON_VERSION);
        println!("Latest release: {} ({})", latest.tag, latest.html_url);
        let keys = source.keyring().ids();
        if keys.is_empty() {
            println!("Release keys:   none in this build; releases can't be verified (see docs/upgrading.md)");
        } else {
            println!("Release keys:   {}", keys.join(", "));
        }
        match aegis_update::plan(&latest.tag, DAEMON_VERSION)? {
            aegis_update::Plan::Upgrade => println!("A newer release is available."),
            aegis_update::Plan::UpToDate => println!("Aegis is up to date."),
            aegis_update::Plan::Ahead => println!("This build is newer than the latest release."),
        }
        return Ok(());
    }

    // --from-request: run by aegis-update.service (root) when the daemon
    // asks for an update. Only the version is taken from the request.
    let request_dir = config.daemon.resolved_data_dir().join("update");
    if from_request {
        // The dashboard can never install an unsigned release.
        if force || wanted.is_some() || allow_unsigned {
            anyhow::bail!(
                "--from-request takes the version from the request; no --force/--version/--allow-unsigned"
            );
        }
        match helper::take_request(&request_dir) {
            Ok(Some(tag)) => wanted = Some(tag),
            Ok(None) => {
                println!("No update request.");
                return Ok(());
            }
            Err(e) => {
                let _ = helper::write_result(&request_dir, false, &e.to_string());
                return Err(e);
            }
        }
    }

    let target = Target::current()
        .filter(|t| t.self_update_supported())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Self-update isn't supported on this platform; download a release from https://github.com/{}/releases",
                config.update.repo
            )
        })?;
    let exe = exe_path();
    if allow_unsigned {
        eprintln!("{}", UNSIGNED_WARNING);
    }
    // The dashboard took its backup before asking the helper (--from-request).
    let backup = PreUpdateBackup {
        config,
        taken: Default::default(),
    };
    let before_swap: Option<&dyn helper::BeforeSwap> = if from_request || no_backup {
        None
    } else {
        Some(&backup)
    };
    let result = helper::self_update(helper::SelfUpdate {
        source: &source,
        exe: &exe,
        target,
        current,
        wanted: wanted.as_deref(),
        force,
        verify_runs: true,
        signatures: if allow_unsigned {
            SignaturePolicy::AllowUnsigned
        } else {
            SignaturePolicy::Required
        },
        before_swap,
    })
    .await;
    let installed = match result {
        Ok(installed) => installed,
        Err(e) => {
            if from_request {
                let _ = helper::write_failure(&request_dir, &e);
            }
            return Err(e);
        }
    };
    match &installed.signed_by {
        Some(key) => println!(
            "Installed Aegis {} (was {}): SHA256SUMS is signed by release key {}, and the archive's SHA-256 {} matches it.",
            installed.tag, installed.from, key, installed.archive_sha256
        ),
        None => eprintln!(
            "Installed Aegis {} (was {}) WITHOUT a signature check (--allow-unsigned); the archive's SHA-256 {} matched the release's SHA256SUMS.",
            installed.tag, installed.from, installed.archive_sha256
        ),
    }
    // For the daemon to record when it starts (UpdateApplied). The update
    // directory is the daemon's; never create it here (as root).
    let record = helper::InstalledRecord {
        version: installed.tag.clone(),
        signed_by: installed.signed_by.clone(),
        allow_unsigned: installed.signed_by.is_none(),
        method: if from_request {
            "helper"
        } else {
            "self-update"
        }
        .into(),
        at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        backup: backup.taken.lock().unwrap().clone(),
    };
    if let Err(e) = helper::write_installed(&request_dir, &record) {
        if allow_unsigned {
            eprintln!(
                "Could not record the unsigned install for the event log ({}).",
                e
            );
        }
    }
    for path in &installed.replaced {
        println!(
            "  replaced {} (previous: {})",
            path.display(),
            aegis_update::install::previous_path(path).display()
        );
    }
    match unit {
        Some(unit) => {
            if let Err(e) = restart_unit(&unit) {
                if from_request {
                    let _ = helper::write_result(&request_dir, true, &e.to_string());
                }
                return Err(e);
            }
            println!("Restarting {}.", unit);
        }
        None => println!(
            "Restart the daemon to run the new version (e.g. sudo systemctl restart aegis)."
        ),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_names() {
        assert!(valid_unit("aegis.service"));
        assert!(valid_unit("aegis@prod.service"));
        for bad in ["", "aegis service", "aegis;reboot", "../x", "a\nb"] {
            assert!(!valid_unit(bad), "{bad:?}");
        }
    }

    #[test]
    fn manual_command_names_the_binary_and_config() {
        let cmd = manual_command(
            Path::new("/usr/local/bin/aegis-daemon"),
            "/etc/aegis/aegis.toml",
        );
        assert!(cmd.starts_with("sudo env AEGIS_CONFIG="), "{cmd}");
        assert!(
            cmd.contains("aegis.toml /usr/local/bin/aegis-daemon self-update"),
            "{cmd}"
        );
    }
}
