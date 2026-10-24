//! Running git for group deployments and update checks: no prompts, no
//! credential helpers, the app's deploy key for SSH remotes, a timeout on
//! every command.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

/// A git environment, holding the deploy key file (if any) while it lives.
pub(crate) struct Git {
    ssh_command: Option<String>,
    _session: Option<aegis_ssh::SshSession>,
}

impl Git {
    pub fn new(ssh: Option<&aegis_ssh::SshAuth>) -> Result<Self, String> {
        let session = match ssh {
            Some(auth) => Some(auth.session()?),
            None => None,
        };
        Ok(Self {
            ssh_command: session.as_ref().map(|s| s.git_ssh_command()),
            _session: session,
        })
    }

    /// Runs `git <args>` in `dir`; returns stdout, or git's error message.
    pub async fn run(
        &self,
        dir: &Path,
        args: &[&str],
        timeout: Duration,
    ) -> Result<String, String> {
        let mut cmd = Command::new("git");
        cmd.args([
            "-c",
            "credential.helper=",
            "-c",
            "core.askPass=",
            // Folders on the server often belong to another user; we only read.
            "-c",
            "safe.directory=*",
        ])
        .args(args)
        .current_dir(dir)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
        if let Some(ssh) = &self.ssh_command {
            cmd.env("GIT_SSH_COMMAND", ssh);
        }
        let output = match tokio::time::timeout(timeout, cmd.output()).await {
            Ok(Ok(output)) => output,
            Ok(Err(e)) => return Err(format!("Failed to run git: {}", e)),
            Err(_) => {
                return Err(format!(
                    "git {} gave no answer within {}s",
                    args.first().unwrap_or(&""),
                    timeout.as_secs()
                ))
            }
        };
        if output.status.success() {
            return Ok(String::from_utf8_lossy(&output.stdout).to_string());
        }
        let mut message = String::from_utf8_lossy(&output.stderr)
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        if message.len() > 1000 {
            let mut cut = 1000;
            while !message.is_char_boundary(cut) {
                cut -= 1;
            }
            message.truncate(cut);
        }
        if message.is_empty() {
            message = format!("git {} exited with {}", args.join(" "), output.status);
        }
        Err(message)
    }
}

/// A full 40-character commit id.
pub(crate) fn is_sha(text: &str) -> bool {
    text.len() == 40 && text.chars().all(|c| c.is_ascii_hexdigit())
}

/// Whether a repository path matches a watch-path glob. `*` and `?` match
/// within one path segment, `**` any number of segments; a pattern without
/// wildcards matches that file or everything under that folder.
pub fn path_matches(pattern: &str, path: &str) -> bool {
    let pattern = pattern
        .trim()
        .trim_start_matches("./")
        .trim_end_matches('/');
    if pattern.is_empty() {
        return false;
    }
    if !pattern.contains(['*', '?']) {
        return path == pattern
            || path
                .strip_prefix(pattern)
                .is_some_and(|rest| rest.starts_with('/'));
    }
    let pat: Vec<&str> = pattern.split('/').collect();
    let parts: Vec<&str> = path.split('/').collect();
    match_segments(&pat, &parts)
}

fn match_segments(pat: &[&str], parts: &[&str]) -> bool {
    match pat.split_first() {
        None => parts.is_empty(),
        Some((&"**", rest)) => (0..=parts.len()).any(|skip| match_segments(rest, &parts[skip..])),
        Some((first, rest)) => match parts.split_first() {
            Some((part, others)) => match_segment(first, part) && match_segments(rest, others),
            None => false,
        },
    }
}

fn match_segment(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// Whether any of `files` concerns an app watching `patterns` (all files
/// when there are none).
pub fn touches(patterns: &[String], files: &[String]) -> bool {
    if patterns.is_empty() {
        return !files.is_empty();
    }
    files
        .iter()
        .any(|f| patterns.iter().any(|p| path_matches(p, f)))
}

/// Checks a watch-path pattern.
pub(crate) fn validate_watch_path(pattern: &str) -> Result<(), String> {
    let p = pattern.trim();
    if p.is_empty() || p.len() > 256 {
        return Err("A watch path must be 1-256 characters".into());
    }
    if p.starts_with('/') || p.contains('\\') || p.contains('\0') {
        return Err(format!(
            "Watch path '{}' must be relative to the repository, with / separators",
            p
        ));
    }
    if p.split('/').any(|s| s == "..") {
        return Err(format!("Watch path '{}' must not contain '..'", p));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watch_path_globs() {
        assert!(path_matches("frontend_v2/**", "frontend_v2/src/app.tsx"));
        assert!(path_matches("frontend_v2/**", "frontend_v2/package.json"));
        assert!(!path_matches("frontend_v2/**", "frontend/src/app.tsx"));
        assert!(path_matches("shared", "shared/util.ts"));
        assert!(path_matches("shared/", "shared/a/b.ts"));
        assert!(!path_matches("shared", "shared-old/util.ts"));
        assert!(path_matches("package.json", "package.json"));
        assert!(path_matches("*.md", "README.md"));
        assert!(!path_matches("*.md", "docs/README.md"));
        assert!(path_matches("**/*.md", "docs/README.md"));
        assert!(path_matches("**/*.md", "README.md"));
        assert!(path_matches("apps/*/src/**", "apps/web/src/x/y.ts"));
        assert!(!path_matches("apps/*/src/**", "apps/web/test/x.ts"));
        assert!(path_matches("back?nd/**", "backend/main.py"));
        assert!(!path_matches("", "anything"));

        let files = vec!["backend/main.py".to_string(), "README.md".to_string()];
        assert!(touches(&[], &files));
        assert!(!touches(&[], &[]));
        assert!(touches(&["backend/**".into()], &files));
        assert!(!touches(
            &["frontend_v2/**".into(), "shared/**".into()],
            &files
        ));
    }

    #[test]
    fn watch_path_validation() {
        assert!(validate_watch_path("frontend_v2/**").is_ok());
        for bad in ["", "/etc/**", "../x", "a/../b", "a\\b"] {
            assert!(validate_watch_path(bad).is_err(), "{bad}");
        }
        assert!(is_sha("0123456789abcdef0123456789abcdef01234567"));
        assert!(!is_sha("main"));
    }
}
