//! Per-app SSH deploy keys for cloning private repositories.
//!
//! * [`generate_deploy_key`] makes an Ed25519 key pair (OpenSSH format).
//!   The private half is stored encrypted by the caller and never leaves the
//!   daemon; the public half is what the operator adds to the repository.
//! * [`RemoteUrl`] understands the URL forms git accepts for SSH and HTTPS
//!   remotes, and converts GitHub HTTPS URLs to their SSH form.
//! * [`SshAuth::session`] writes the private key to a 0600 file in a fresh
//!   0700 directory for the duration of one git command, with an
//!   Aegis-managed `known_hosts` file. Dropping the [`SshSession`] deletes
//!   both, whether the command succeeded or not.
//!
//! Host keys are never trusted on first use: the session's `known_hosts`
//! holds GitHub's published keys ([`GITHUB_KNOWN_HOSTS`]) plus the contents of
//! an operator-provided file (`[git] known_hosts_file`), and ssh runs with
//! `StrictHostKeyChecking=yes`.

use rand_core::{OsRng, RngCore};
use ssh_key::{Algorithm, HashAlg, LineEnding, PrivateKey};
use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};
use zeroize::Zeroizing;

/// GitHub's SSH host keys, as published at
/// <https://docs.github.com/en/authentication/keeping-your-account-and-data-secure/githubs-ssh-key-fingerprints>
/// and in the `ssh_keys` field of <https://api.github.com/meta>
/// (see <https://docs.github.com/en/rest/meta/meta>). If GitHub ever rotates
/// a key, update this file (and the fingerprints in the tests below).
pub const GITHUB_KNOWN_HOSTS: &str = include_str!("github_known_hosts");

/// A freshly generated deploy key.
pub struct DeployKey {
    /// OpenSSH private key (PEM-like `-----BEGIN OPENSSH PRIVATE KEY-----`).
    pub private_openssh: Zeroizing<String>,
    /// One line: `ssh-ed25519 AAAA... comment`.
    pub public_openssh: String,
    /// `SHA256:...`, as `ssh-keygen -l` and GitHub show it.
    pub fingerprint: String,
}

impl fmt::Debug for DeployKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeployKey")
            .field("public_openssh", &self.public_openssh)
            .field("fingerprint", &self.fingerprint)
            .finish_non_exhaustive()
    }
}

/// Generates an Ed25519 key pair. `comment` ends up in the public key line
/// (e.g. `aegis:my-app`); it is not secret.
pub fn generate_deploy_key(comment: &str) -> Result<DeployKey, String> {
    let mut key = PrivateKey::random(&mut OsRng, Algorithm::Ed25519)
        .map_err(|e| format!("Key generation failed: {}", e))?;
    key.set_comment(comment);
    let private_openssh = key
        .to_openssh(LineEnding::LF)
        .map_err(|e| format!("Key encoding failed: {}", e))?;
    let public = key.public_key();
    Ok(DeployKey {
        private_openssh: Zeroizing::new(private_openssh.to_string()),
        public_openssh: public
            .to_openssh()
            .map_err(|e| format!("Key encoding failed: {}", e))?,
        fingerprint: public.fingerprint(HashAlg::Sha256).to_string(),
    })
}

/// The public key line and fingerprint of a stored private key.
pub fn public_of(private_openssh: &str) -> Result<(String, String), String> {
    let key = PrivateKey::from_openssh(private_openssh)
        .map_err(|_| "The stored deploy key is not a valid OpenSSH private key".to_string())?;
    let public = key.public_key();
    Ok((
        public
            .to_openssh()
            .map_err(|e| format!("Key encoding failed: {}", e))?,
        public.fingerprint(HashAlg::Sha256).to_string(),
    ))
}

// ----------------------------------------------------------------------
// Remote URLs
// ----------------------------------------------------------------------

/// How git reaches a remote.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// `ssh://[user@]host[:port]/path` or scp-like `[user@]host:path`.
    Ssh,
    /// `https://` or `http://`.
    Https,
}

/// A network git remote. Local paths and `file://` URLs are not one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteUrl {
    pub transport: Transport,
    /// The user for SSH (`git` for GitHub); never the HTTPS credentials.
    pub user: Option<String>,
    /// Lower-cased host name.
    pub host: String,
    pub port: Option<u16>,
    /// The path after the host, without a leading `/`.
    pub path: String,
}

fn valid_host(host: &str) -> bool {
    !host.is_empty()
        && host.len() <= 253
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-'))
}

fn valid_name(part: &str) -> bool {
    !part.is_empty()
        && part.len() <= 100
        && part != "."
        && part != ".."
        && part
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

impl RemoteUrl {
    /// Parses the URL forms git accepts for SSH and HTTP(S) remotes.
    /// Returns `None` for anything else (local paths, `file://`, `git://`).
    pub fn parse(url: &str) -> Option<Self> {
        let url = url.trim();
        if let Some(rest) = url
            .strip_prefix("https://")
            .map(|r| (Transport::Https, r))
            .or_else(|| url.strip_prefix("http://").map(|r| (Transport::Https, r)))
            .or_else(|| url.strip_prefix("ssh://").map(|r| (Transport::Ssh, r)))
            .or_else(|| url.strip_prefix("git+ssh://").map(|r| (Transport::Ssh, r)))
        {
            let (transport, rest) = rest;
            let (authority, path) = rest.split_once('/')?;
            let (userinfo, hostport) = match authority.rsplit_once('@') {
                Some((u, h)) => (Some(u), h),
                None => (None, authority),
            };
            let (host, port) = match hostport.rsplit_once(':') {
                Some((h, p)) => (h, Some(p.parse::<u16>().ok()?)),
                None => (hostport, None),
            };
            if !valid_host(host) {
                return None;
            }
            let user = match transport {
                Transport::Ssh => userinfo.map(str::to_string),
                // Credentials in an HTTPS URL are not a user name to keep.
                Transport::Https => None,
            };
            return Some(Self {
                transport,
                user,
                host: host.to_ascii_lowercase(),
                port,
                path: path.trim_start_matches('/').to_string(),
            });
        }
        if url.contains("://") {
            return None;
        }
        // scp-like: [user@]host:path. A colon after a slash is a local path.
        let colon = url.find(':')?;
        if url[..colon].contains('/') {
            return None;
        }
        let (userhost, path) = url.split_at(colon);
        let path = &path[1..];
        let (user, host) = match userhost.rsplit_once('@') {
            Some((u, h)) => (Some(u.to_string()), h),
            None => (None, userhost),
        };
        // `C:` (a Windows drive) is a path, not a host.
        if !valid_host(host) || host.len() == 1 || path.is_empty() {
            return None;
        }
        Some(Self {
            transport: Transport::Ssh,
            user,
            host: host.to_ascii_lowercase(),
            port: None,
            path: path.trim_start_matches('/').to_string(),
        })
    }

    pub fn is_ssh(&self) -> bool {
        self.transport == Transport::Ssh
    }

    /// `(owner, repo)` for a `github.com` repository URL; `repo` has no
    /// `.git` suffix.
    pub fn github_repo(&self) -> Option<(String, String)> {
        if self.host != "github.com" && self.host != "www.github.com" {
            return None;
        }
        let path = self.path.trim_end_matches('/');
        let path = path.strip_suffix(".git").unwrap_or(path);
        let (owner, repo) = path.split_once('/')?;
        (valid_name(owner) && valid_name(repo) && !repo.contains('/'))
            .then(|| (owner.to_string(), repo.to_string()))
    }
}

/// `git@github.com:owner/repo.git` for any form of a GitHub repository URL.
pub fn github_ssh_url(url: &str) -> Option<String> {
    let (owner, repo) = RemoteUrl::parse(url)?.github_repo()?;
    Some(format!("git@github.com:{}/{}.git", owner, repo))
}

/// `https://github.com/owner/repo.git` for any form of a GitHub repository
/// URL (for an unauthenticated visibility check).
pub fn github_https_url(url: &str) -> Option<String> {
    let (owner, repo) = RemoteUrl::parse(url)?.github_repo()?;
    Some(format!("https://github.com/{}/{}.git", owner, repo))
}

/// Where a repository's deploy keys are added on GitHub.
pub fn github_deploy_keys_page(url: &str) -> Option<String> {
    let (owner, repo) = RemoteUrl::parse(url)?.github_repo()?;
    Some(format!(
        "https://github.com/{}/{}/settings/keys/new",
        owner, repo
    ))
}

/// The URL to clone with when a deploy key is available: SSH URLs as they
/// are, GitHub HTTPS URLs converted to SSH (deploy keys only work over SSH).
/// `None` when the key can't be used for this URL (another host over HTTPS,
/// local paths).
pub fn url_for_deploy_key(url: &str) -> Option<String> {
    let remote = RemoteUrl::parse(url)?;
    match remote.transport {
        Transport::Ssh => Some(url.trim().to_string()),
        Transport::Https => github_ssh_url(url),
    }
}

// ----------------------------------------------------------------------
// Key files for one git command
// ----------------------------------------------------------------------

/// Quotes `text` for a POSIX shell (git runs `GIT_SSH_COMMAND` through one).
pub fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// The `GIT_SSH_COMMAND` that makes git use exactly this key and these host
/// keys. Only paths appear in it, never key material.
pub fn git_ssh_command(key_path: &Path, known_hosts: &Path) -> String {
    // `AEGIS_SSH_BIN` replaces the ssh client (tests use a stand-in).
    let program = match std::env::var("AEGIS_SSH_BIN") {
        Ok(bin) if !bin.is_empty() => shell_quote(&bin),
        _ => "ssh".to_string(),
    };
    format!(
        "{} -i {} -o IdentitiesOnly=yes -o IdentityAgent=none -o BatchMode=yes \
         -o StrictHostKeyChecking=yes -o UserKnownHostsFile={} -o ConnectTimeout=20",
        program,
        shell_quote(&key_path.to_string_lossy()),
        shell_quote(&known_hosts.to_string_lossy())
    )
}

/// A deploy key ready to be used for git commands. `Debug` never shows the key.
#[derive(Clone)]
pub struct SshAuth {
    private_key: Zeroizing<String>,
    /// `SHA256:...` of the key, for logs.
    pub fingerprint: String,
    /// Where the short-lived key directories are created (`<data_dir>/run/ssh`).
    pub run_dir: PathBuf,
    /// Extra host keys (`[git] known_hosts_file`), appended to GitHub's.
    pub extra_known_hosts: Option<PathBuf>,
}

impl fmt::Debug for SshAuth {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SshAuth")
            .field("fingerprint", &self.fingerprint)
            .field("run_dir", &self.run_dir)
            .field("extra_known_hosts", &self.extra_known_hosts)
            .finish_non_exhaustive()
    }
}

/// The key and `known_hosts` files of one git command. Dropping it deletes
/// them.
#[derive(Debug)]
pub struct SshSession {
    dir: PathBuf,
    key_path: PathBuf,
    known_hosts: PathBuf,
}

impl SshSession {
    /// The value for `GIT_SSH_COMMAND`.
    pub fn git_ssh_command(&self) -> String {
        git_ssh_command(&self.key_path, &self.known_hosts)
    }

    pub fn key_path(&self) -> &Path {
        &self.key_path
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

impl Drop for SshSession {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn create_private_dir(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().mode(0o700).create(path)
    }
    #[cfg(not(unix))]
    std::fs::DirBuilder::new().create(path)
}

fn write_private_file(path: &Path, content: &[u8]) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(content)?;
    file.sync_all()
}

/// The `known_hosts` text a session uses: GitHub's keys, then the extra file.
pub fn known_hosts_text(extra: Option<&Path>) -> Result<String, String> {
    let mut text = GITHUB_KNOWN_HOSTS.to_string();
    if let Some(path) = extra {
        let more = std::fs::read_to_string(path).map_err(|e| {
            format!(
                "Cannot read the host keys file {} ([git] known_hosts_file): {}",
                path.display(),
                e
            )
        })?;
        if !text.ends_with('\n') {
            text.push('\n');
        }
        text.push_str(&more);
    }
    Ok(text)
}

impl SshAuth {
    pub fn new(
        private_key: Zeroizing<String>,
        fingerprint: String,
        run_dir: PathBuf,
        extra_known_hosts: Option<PathBuf>,
    ) -> Self {
        Self {
            private_key,
            fingerprint,
            run_dir,
            extra_known_hosts,
        }
    }

    /// Writes the key (0600) and `known_hosts` into a new 0700 directory
    /// under `run_dir`. Keep the session alive while git runs.
    pub fn session(&self) -> Result<SshSession, String> {
        let known_hosts = known_hosts_text(self.extra_known_hosts.as_deref())?;
        std::fs::create_dir_all(&self.run_dir).map_err(|e| {
            format!(
                "Cannot create {} for the deploy key: {}",
                self.run_dir.display(),
                e
            )
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&self.run_dir, std::fs::Permissions::from_mode(0o700));
        }
        let mut id = [0u8; 12];
        OsRng.fill_bytes(&mut id);
        let name: String = id.iter().map(|b| format!("{:02x}", b)).collect();
        let dir = self.run_dir.join(format!("key-{}", name));
        create_private_dir(&dir).map_err(|e| format!("Cannot create {}: {}", dir.display(), e))?;
        // From here on the directory is removed when `session` drops.
        let session = SshSession {
            key_path: dir.join("id_ed25519"),
            known_hosts: dir.join("known_hosts"),
            dir,
        };
        let mut key = Zeroizing::new(self.private_key.as_bytes().to_vec());
        if !key.ends_with(b"\n") {
            key.push(b'\n');
        }
        write_private_file(&session.key_path, &key)
            .map_err(|e| format!("Cannot write the deploy key file: {}", e))?;
        write_private_file(&session.known_hosts, known_hosts.as_bytes())
            .map_err(|e| format!("Cannot write the known_hosts file: {}", e))?;
        Ok(session)
    }
}

/// Removes key directories left behind by a crash (call at startup).
pub fn clean_run_dir(run_dir: &Path) {
    if let Ok(entries) = std::fs::read_dir(run_dir) {
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().starts_with("key-") {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_keys_round_trip() {
        let key = generate_deploy_key("aegis:web").unwrap();
        assert!(key
            .private_openssh
            .starts_with("-----BEGIN OPENSSH PRIVATE KEY-----"));
        assert!(key.public_openssh.starts_with("ssh-ed25519 AAAA"));
        assert!(key.public_openssh.ends_with(" aegis:web"));
        assert!(key.fingerprint.starts_with("SHA256:"));
        let (public, fingerprint) = public_of(&key.private_openssh).unwrap();
        assert_eq!(public, key.public_openssh);
        assert_eq!(fingerprint, key.fingerprint);
        // Debug output never shows the private key.
        let debug = format!("{:?}", key);
        assert!(!debug.contains("PRIVATE"), "{debug}");
        assert!(public_of("not a key").is_err());
        assert_ne!(
            generate_deploy_key("x").unwrap().fingerprint,
            key.fingerprint
        );
    }

    #[test]
    fn pinned_github_host_keys_match_published_fingerprints() {
        // https://docs.github.com/en/authentication/keeping-your-account-and-data-secure/githubs-ssh-key-fingerprints
        let expected = [
            "SHA256:+DiY3wvvV6TuJJhbpZisF/zLDA0zPMSvHdkr4UvCOqU",
            "SHA256:p2QAMXNIC1TJYWeIOttrVc98/R1BUFWu3/LiyKgUfQM",
            "SHA256:uNiVztksCsDhcc0u9e8BujQXVUpKZIDTMczCvj3tD2s",
        ];
        let lines: Vec<&str> = GITHUB_KNOWN_HOSTS.lines().collect();
        assert_eq!(lines.len(), expected.len());
        for (line, want) in lines.iter().zip(expected) {
            let key = line.strip_prefix("github.com ").unwrap();
            let public = ssh_key::PublicKey::from_openssh(key).unwrap();
            assert_eq!(public.fingerprint(HashAlg::Sha256).to_string(), want);
        }
    }

    #[test]
    fn remote_urls() {
        let p = |u: &str| RemoteUrl::parse(u);
        let scp = p("git@github.com:Owner/repo.git").unwrap();
        assert_eq!(scp.transport, Transport::Ssh);
        assert_eq!(scp.user.as_deref(), Some("git"));
        assert_eq!(scp.github_repo(), Some(("Owner".into(), "repo".into())));
        let ssh = p("ssh://git@GitHub.com:22/o/r").unwrap();
        assert_eq!(ssh.host, "github.com");
        assert_eq!(ssh.port, Some(22));
        assert_eq!(ssh.github_repo(), Some(("o".into(), "r".into())));
        let https = p("https://user:tok@github.com/o/r.git/").unwrap();
        assert_eq!(https.transport, Transport::Https);
        assert_eq!(https.user, None);
        assert_eq!(https.github_repo(), Some(("o".into(), "r".into())));
        assert!(p("https://gitlab.com/o/r.git")
            .unwrap()
            .github_repo()
            .is_none());
        assert!(p("https://github.com/o").unwrap().github_repo().is_none());
        assert!(p("https://github.com/o/r/tree/main")
            .unwrap()
            .github_repo()
            .is_none());
        for local in [
            "/srv/repos/app",
            "./app",
            "file:///srv/app",
            "C:\\repos\\app",
            "C:/repos/app",
            "git://host/x",
            "",
            "dir/with:colon",
        ] {
            assert_eq!(p(local), None, "{local}");
        }
        assert_eq!(
            github_ssh_url("https://github.com/o/r").as_deref(),
            Some("git@github.com:o/r.git")
        );
        assert_eq!(
            github_https_url("git@github.com:o/r.git").as_deref(),
            Some("https://github.com/o/r.git")
        );
        assert_eq!(
            github_deploy_keys_page("git@github.com:o/r.git").as_deref(),
            Some("https://github.com/o/r/settings/keys/new")
        );
        // SSH URLs are kept; GitHub HTTPS converted; other HTTPS can't use a key.
        assert_eq!(
            url_for_deploy_key("ssh://git@git.example.com:2222/a/b.git").as_deref(),
            Some("ssh://git@git.example.com:2222/a/b.git")
        );
        assert_eq!(
            url_for_deploy_key("https://github.com/o/r.git").as_deref(),
            Some("git@github.com:o/r.git")
        );
        assert_eq!(url_for_deploy_key("https://gitlab.com/o/r.git"), None);
        assert_eq!(url_for_deploy_key("/srv/app"), None);
    }

    #[test]
    fn ssh_command_has_paths_only() {
        let cmd = git_ssh_command(Path::new("/d/it's/id"), Path::new("/d/kh"));
        assert!(cmd.starts_with("ssh -i '/d/it'\\''s/id' "), "{cmd}");
        assert!(cmd.contains("-o IdentitiesOnly=yes"));
        assert!(cmd.contains("-o StrictHostKeyChecking=yes"));
        assert!(cmd.contains("-o UserKnownHostsFile='/d/kh'"));
    }

    #[test]
    fn session_files_exist_only_while_the_session_lives() {
        let tmp = tempfile::TempDir::new().unwrap();
        let key = generate_deploy_key("t").unwrap();
        let extra = tmp.path().join("extra_known_hosts");
        std::fs::write(&extra, "git.example.com ssh-ed25519 AAAAexample\n").unwrap();
        let auth = SshAuth::new(
            key.private_openssh.clone(),
            key.fingerprint.clone(),
            tmp.path().join("run"),
            Some(extra),
        );
        // Debug never shows the key.
        assert!(!format!("{:?}", auth).contains("PRIVATE"));
        let session = auth.session().unwrap();
        let dir = session.dir().to_path_buf();
        let written = std::fs::read_to_string(session.key_path()).unwrap();
        assert_eq!(written.trim_end(), key.private_openssh.trim_end());
        let hosts = std::fs::read_to_string(dir.join("known_hosts")).unwrap();
        assert!(hosts.contains("github.com ssh-ed25519"));
        assert!(hosts.contains("git.example.com"));
        let command = session.git_ssh_command();
        assert!(command.contains(&*session.key_path().to_string_lossy()));
        assert!(!command.contains("PRIVATE"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&dir), 0o700);
            assert_eq!(mode(session.key_path()), 0o600);
        }
        drop(session);
        assert!(!dir.exists());

        // A missing extra host keys file is an error, and leaves nothing behind.
        let broken = SshAuth::new(
            key.private_openssh.clone(),
            key.fingerprint,
            tmp.path().join("run"),
            Some(tmp.path().join("missing")),
        );
        assert!(broken.session().unwrap_err().contains("known_hosts_file"));
        assert_eq!(
            std::fs::read_dir(tmp.path().join("run")).unwrap().count(),
            0
        );

        // Leftovers from a crash are swept.
        let leftover = tmp.path().join("run").join("key-dead");
        std::fs::create_dir_all(&leftover).unwrap();
        clean_run_dir(&tmp.path().join("run"));
        assert!(!leftover.exists());
    }
}
