//! Server-side env files: a per-app `.env` the daemon reads at every build
//! and every process start (dashboard Settings → Env file).
//!
//! The syntax is the usual dotenv one:
//!
//! ```text
//! # comment
//! NAME=value                 # unquoted; " #" starts a trailing comment
//! export NAME=value          # `export ` is ignored
//! NAME='literal $value'      # single quotes: no escapes, may span lines
//! NAME="a\nb \"quoted\""     # double quotes: \n \r \t \" \\ \$ escapes, may span lines
//! ```
//!
//! There is no variable expansion: `${OTHER}` stays as written.
//!
//! Values may be secrets (database URLs, private keys), so nothing here ever
//! puts a value into an error message or a `Debug` output: errors name the
//! file and the line number only.
//!
//! Files are only read from directories the daemon configuration allows
//! (`[apps] env_file_dirs`), after resolving symbolic links, and only
//! regular files of at most [`MAX_ENV_FILE_BYTES`].

use std::collections::BTreeMap;
use std::fmt;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

/// Env files larger than this are refused (1 MiB).
pub const MAX_ENV_FILE_BYTES: u64 = 1024 * 1024;
/// Longest accepted path setting.
const MAX_PATH_LEN: usize = 4096;
/// Longest variable name (as for dashboard variables and secrets).
const MAX_NAME_LEN: usize = 128;

/// Variables read from an env file. `Debug` lists the names only.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct EnvVars(BTreeMap<String, String>);

impl EnvVars {
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.0.keys().map(String::as_str)
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        self.0.get(name).map(String::as_str)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn into_inner(self) -> BTreeMap<String, String> {
        self.0
    }
}

impl fmt::Debug for EnvVars {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_set().entries(self.0.keys()).finish()
    }
}

/// A syntax error. The message never contains any part of the line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    /// 1-based line number.
    pub line: usize,
    pub message: &'static str,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

impl std::error::Error for ParseError {}

/// Whether `name` is usable as an environment variable name.
pub fn is_valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c == '_' || c.is_ascii_alphabetic())
        && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
        && name.len() <= MAX_NAME_LEN
}

fn error(line: usize, message: &'static str) -> ParseError {
    ParseError { line, message }
}

/// `export NAME=...` -> `NAME=...`.
fn strip_export(line: &str) -> &str {
    match line.strip_prefix("export") {
        Some(rest) if rest.starts_with([' ', '\t']) => rest.trim_start(),
        _ => line,
    }
}

/// An unquoted value: up to a `#` that follows whitespace, trimmed.
fn unquoted(after_eq: &str) -> &str {
    let bytes = after_eq.as_bytes();
    // `NAME=#x` keeps "#x": a comment needs whitespace before it.
    let end = (1..bytes.len())
        .find(|&i| bytes[i] == b'#' && matches!(bytes[i - 1], b' ' | b'\t'))
        .unwrap_or(bytes.len());
    after_eq[..end].trim()
}

/// Parses env file text. Later definitions of a name replace earlier ones.
pub fn parse(text: &str) -> Result<EnvVars, ParseError> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let lines: Vec<&str> = text
        .split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .collect();
    let mut vars = BTreeMap::new();
    let mut i = 0;
    while i < lines.len() {
        let line_no = i + 1;
        let line = lines[i].trim_start();
        i += 1;
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = strip_export(line);
        let Some((name, after_eq)) = line.split_once('=') else {
            return Err(error(line_no, "expected NAME=value"));
        };
        let name = name.trim();
        if !is_valid_name(name) {
            return Err(error(
                line_no,
                "invalid variable name (use letters, digits and _, not starting with a digit, at most 128 characters)",
            ));
        }
        let value_start = after_eq.trim_start_matches([' ', '\t']);
        let value = match value_start.chars().next() {
            Some(quote @ ('"' | '\'')) => {
                let mut value = String::new();
                let mut rest = &value_start[1..];
                loop {
                    match scan_quoted(rest, quote, &mut value) {
                        Some(after) => {
                            let after = after.trim_start();
                            if !(after.is_empty() || after.starts_with('#')) {
                                return Err(error(
                                    line_no,
                                    "unexpected characters after the closing quote",
                                ));
                            }
                            break;
                        }
                        None if i < lines.len() => {
                            value.push('\n');
                            rest = lines[i];
                            i += 1;
                        }
                        None => {
                            return Err(error(
                                line_no,
                                if quote == '"' {
                                    "unterminated double-quoted value"
                                } else {
                                    "unterminated single-quoted value"
                                },
                            ))
                        }
                    }
                }
                value
            }
            _ => unquoted(after_eq).to_string(),
        };
        if value.contains('\0') {
            return Err(error(line_no, "values can't contain NUL characters"));
        }
        vars.insert(name.to_string(), value);
    }
    Ok(EnvVars(vars))
}

/// Appends a quoted value's characters from `text` to `out`. Returns the
/// text after the closing quote, or `None` if the line ends first.
fn scan_quoted<'a>(text: &'a str, quote: char, out: &mut String) -> Option<&'a str> {
    let mut chars = text.char_indices();
    while let Some((i, c)) = chars.next() {
        if c == quote {
            return Some(&text[i + 1..]);
        }
        if c == '\\' && quote == '"' {
            match chars.next() {
                Some((_, 'n')) => out.push('\n'),
                Some((_, 'r')) => out.push('\r'),
                Some((_, 't')) => out.push('\t'),
                Some((_, e @ ('"' | '\\' | '$'))) => out.push(e),
                Some((_, other)) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
            continue;
        }
        out.push(c);
    }
    None
}

/// Why an env file can't be used. Never holds file contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvFileError {
    /// `[apps] env_file_dirs` is empty.
    Disabled,
    Invalid(&'static str),
    OutsideAllowed {
        path: String,
        dirs: Vec<PathBuf>,
    },
    NotFound(String),
    PermissionDenied {
        path: String,
        user: String,
    },
    NotRegular(String),
    TooBig(String),
    NotUtf8(String),
    Io {
        path: String,
        message: String,
    },
    Parse {
        path: String,
        error: ParseError,
    },
}

impl fmt::Display for EnvFileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Disabled => write!(
                f,
                "Env files are turned off on this server: list the directories they may be read from in the daemon configuration, e.g. [apps] env_file_dirs = [\"/opt/apps\"]"
            ),
            Self::Invalid(why) => f.write_str(why),
            Self::OutsideAllowed { path, dirs } => write!(
                f,
                "{} is not inside a directory allowed by [apps] env_file_dirs ({}); symbolic links are resolved before this check",
                path,
                dirs.iter()
                    .map(|d| d.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::NotFound(path) => write!(f, "{} does not exist", path),
            Self::PermissionDenied { path, user } => write!(
                f,
                "{user} can't read {path}: permission denied. Give the daemon's user read access, e.g. `sudo chgrp {user} {path} && sudo chmod 640 {path}` (it must also be able to enter every parent directory)"
            ),
            Self::NotRegular(path) => write!(f, "{} is not a regular file", path),
            Self::TooBig(path) => write!(
                f,
                "{} is larger than {} KiB",
                path,
                MAX_ENV_FILE_BYTES / 1024
            ),
            Self::NotUtf8(path) => write!(f, "{} is not UTF-8 text", path),
            Self::Io { path, message } => write!(f, "Cannot read {}: {}", path, message),
            Self::Parse { path, error } => write!(f, "{}, {}", path, error),
        }
    }
}

impl std::error::Error for EnvFileError {}

/// The user the daemon runs as, for error messages.
pub fn daemon_user() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("LOGNAME"))
        .or_else(|_| std::env::var("USERNAME"))
        .ok()
        .filter(|u| {
            !u.is_empty()
                && u.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "_-.".contains(c))
        })
        .unwrap_or_else(|| "aegis".to_string())
}

fn io_error(path: &str, e: std::io::Error) -> EnvFileError {
    match e.kind() {
        std::io::ErrorKind::NotFound => EnvFileError::NotFound(path.to_string()),
        std::io::ErrorKind::PermissionDenied => EnvFileError::PermissionDenied {
            path: path.to_string(),
            user: daemon_user(),
        },
        _ => EnvFileError::Io {
            path: path.to_string(),
            message: e.to_string(),
        },
    }
}

/// Checks the form of a path setting: absolute, no `..`, one line.
pub fn validate_path_syntax(path: &str) -> Result<(), EnvFileError> {
    if path.is_empty() || path.len() > MAX_PATH_LEN || path.contains(['\0', '\n', '\r']) {
        return Err(EnvFileError::Invalid(
            "The env file path must be a single line of at most 4096 characters",
        ));
    }
    let p = Path::new(path);
    if !p.is_absolute() {
        return Err(EnvFileError::Invalid(
            "The env file path must be absolute, e.g. /opt/apps/my-app/.env",
        ));
    }
    if p.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(EnvFileError::Invalid(
            "The env file path must not contain '..'",
        ));
    }
    Ok(())
}

/// The allowed directories, resolved (those that don't exist are skipped).
fn resolved_dirs(allowed_dirs: &[PathBuf]) -> Vec<PathBuf> {
    allowed_dirs
        .iter()
        .filter_map(|d| std::fs::canonicalize(d).ok())
        .collect()
}

fn inside(path: &Path, dirs: &[PathBuf]) -> bool {
    dirs.iter().any(|d| path != d && path.starts_with(d))
}

/// Resolves `path` (symbolic links included) and checks that it lies inside
/// one of `allowed_dirs`. Returns the resolved path.
pub fn check_path(path: &str, allowed_dirs: &[PathBuf]) -> Result<PathBuf, EnvFileError> {
    if allowed_dirs.is_empty() {
        return Err(EnvFileError::Disabled);
    }
    validate_path_syntax(path)?;
    let dirs = resolved_dirs(allowed_dirs);
    let outside = || EnvFileError::OutsideAllowed {
        path: path.to_string(),
        dirs: allowed_dirs.to_vec(),
    };
    match std::fs::canonicalize(path) {
        Ok(resolved) if inside(&resolved, &dirs) => Ok(resolved),
        Ok(_) => Err(outside()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Say "outside" rather than "missing" when the directory it
            // would be in is not allowed anyway.
            let p = Path::new(path);
            let resolved_parent = p
                .parent()
                .and_then(|parent| std::fs::canonicalize(parent).ok());
            match (resolved_parent, p.file_name()) {
                (Some(parent), Some(name)) if !inside(&parent.join(name), &dirs) => Err(outside()),
                _ => Err(io_error(path, e)),
            }
        }
        Err(e) => Err(io_error(path, e)),
    }
}

/// Checks an env file setting before it is saved. The file may still be
/// missing or unreadable (that shows in the app's environment overview and
/// fails builds and starts until fixed), but it must be allowed.
pub fn validate_setting(path: &str, allowed_dirs: &[PathBuf]) -> Result<(), EnvFileError> {
    match check_path(path, allowed_dirs) {
        Ok(_) => Ok(()),
        Err(
            e @ (EnvFileError::Disabled
            | EnvFileError::Invalid(_)
            | EnvFileError::OutsideAllowed { .. }),
        ) => Err(e),
        Err(_) => Ok(()),
    }
}

/// Reads and parses an env file, if it is allowed (see [`check_path`]), a
/// regular file and at most [`MAX_ENV_FILE_BYTES`].
pub fn read_env_file(path: &str, allowed_dirs: &[PathBuf]) -> Result<EnvVars, EnvFileError> {
    let resolved = check_path(path, allowed_dirs)?;
    let meta = std::fs::metadata(&resolved).map_err(|e| io_error(path, e))?;
    if !meta.is_file() {
        return Err(EnvFileError::NotRegular(path.to_string()));
    }
    if meta.len() > MAX_ENV_FILE_BYTES {
        return Err(EnvFileError::TooBig(path.to_string()));
    }
    let file = std::fs::File::open(&resolved).map_err(|e| io_error(path, e))?;
    let mut bytes = Vec::new();
    file.take(MAX_ENV_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| io_error(path, e))?;
    if bytes.len() as u64 > MAX_ENV_FILE_BYTES {
        return Err(EnvFileError::TooBig(path.to_string()));
    }
    let text = String::from_utf8(bytes).map_err(|_| EnvFileError::NotUtf8(path.to_string()))?;
    parse(&text).map_err(|error| EnvFileError::Parse {
        path: path.to_string(),
        error,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(text: &str) -> BTreeMap<String, String> {
        parse(text).unwrap().into_inner()
    }

    fn one(text: &str) -> String {
        let v = vars(text);
        assert_eq!(v.len(), 1, "{text:?} -> {:?}", v.keys().collect::<Vec<_>>());
        v.into_values().next().unwrap()
    }

    #[test]
    fn plain_lines_comments_and_export() {
        let v = vars(
            "# a comment\n\n   # indented comment\nA=1\nexport B=two\n  C = three  \nexport\tD=4\r\nE=\n",
        );
        assert_eq!(v["A"], "1");
        assert_eq!(v["B"], "two");
        assert_eq!(v["C"], "three");
        assert_eq!(v["D"], "4");
        assert_eq!(v["E"], "");
        assert_eq!(v.len(), 5);
        // A variable may be called "export".
        assert_eq!(vars("export=1")["export"], "1");
        // Byte order mark.
        assert_eq!(vars("\u{feff}X=1")["X"], "1");
    }

    #[test]
    fn unquoted_values_and_trailing_comments() {
        assert_eq!(one("A=value # comment"), "value");
        assert_eq!(one("A=value\t# comment"), "value");
        assert_eq!(one("A=http://x/#anchor"), "http://x/#anchor");
        assert_eq!(one("A=#not-a-comment"), "#not-a-comment");
        assert_eq!(one("A=#kept #dropped"), "#kept");
        assert_eq!(one("A= # only a comment"), "");
        assert_eq!(one("A=a=b=c"), "a=b=c");
        assert_eq!(one("A=it's \"fine\""), "it's \"fine\"");
        assert_eq!(one("A=  spaced   out  "), "spaced   out");
        assert_eq!(one("A=back\\slash"), "back\\slash");
    }

    #[test]
    fn single_quotes_are_literal() {
        assert_eq!(one("A='a # b'"), "a # b");
        assert_eq!(one("A='$HOME \\n \"x\"' # comment"), "$HOME \\n \"x\"");
        assert_eq!(one("A=''"), "");
        assert_eq!(one("A='line1\nline2'"), "line1\nline2");
    }

    #[test]
    fn double_quotes_handle_escapes_and_lines() {
        assert_eq!(one(r#"A="a\nb""#), "a\nb");
        assert_eq!(one(r#"A="tab\there\r""#), "tab\there\r");
        assert_eq!(one(r#"A="say \"hi\"""#), "say \"hi\"");
        assert_eq!(one(r#"A="back\\slash""#), "back\\slash");
        assert_eq!(one(r#"A="\$5 \q""#), "$5 \\q");
        assert_eq!(one(r#"A="a # b" # comment"#), "a # b");
        assert_eq!(one(r#"A="""#), "");
        // A PEM key over several lines, followed by more variables.
        let v = vars(
            "KEY=\"-----BEGIN PRIVATE KEY-----\r\nMIIabc\r\n-----END PRIVATE KEY-----\"\nNEXT=1\n",
        );
        assert_eq!(
            v["KEY"],
            "-----BEGIN PRIVATE KEY-----\nMIIabc\n-----END PRIVATE KEY-----"
        );
        assert_eq!(v["NEXT"], "1");
        // The usual one-line form with \n escapes.
        assert_eq!(
            one(r#"GCP_KEY="-----BEGIN PRIVATE KEY-----\nMIIabc\n-----END PRIVATE KEY-----\n""#),
            "-----BEGIN PRIVATE KEY-----\nMIIabc\n-----END PRIVATE KEY-----\n"
        );
    }

    #[test]
    fn later_definitions_win() {
        let v = vars("A=1\nA=2\n");
        assert_eq!(v["A"], "2");
    }

    #[test]
    fn errors_name_the_line_but_never_the_value() {
        const SENTINEL: &str = "s3ntinel-Value";
        let cases = [
            (format!("OK=1\n{SENTINEL}\n"), 2, "expected NAME=value"),
            (format!("1BAD={SENTINEL}"), 1, "invalid variable name"),
            (
                format!("postgres://u:{SENTINEL}@h=x"),
                1,
                "invalid variable name",
            ),
            (format!("A B={SENTINEL}"), 1, "invalid variable name"),
            (
                format!("\n\nA=\"{SENTINEL}\nstill open"),
                3,
                "unterminated double",
            ),
            (format!("A='{SENTINEL}"), 1, "unterminated single"),
            (
                format!("A=\"{SENTINEL}\" trailing"),
                1,
                "after the closing quote",
            ),
            (format!("A='x'{SENTINEL}"), 1, "after the closing quote"),
            (format!("A={SENTINEL}\0"), 1, "NUL"),
            (format!("={SENTINEL}"), 1, "invalid variable name"),
        ];
        for (text, line, message) in cases {
            let err = parse(&text).unwrap_err();
            assert_eq!(err.line, line, "{err}");
            let shown = err.to_string();
            assert!(shown.contains(message), "{shown}");
            assert!(!shown.contains(SENTINEL), "{shown}");
            assert!(!format!("{err:?}").contains(SENTINEL));
        }
    }

    #[test]
    fn debug_shows_names_only() {
        let v = parse("PASSWORD=hunter2").unwrap();
        let shown = format!("{v:?}");
        assert!(shown.contains("PASSWORD"));
        assert!(!shown.contains("hunter2"));
    }

    #[test]
    fn path_syntax() {
        let dirs = [std::env::temp_dir()];
        assert_eq!(check_path("/x/.env", &[]), Err(EnvFileError::Disabled));
        for bad in ["", "relative/.env", "/opt/../etc/shadow", "/opt/a\n.env"] {
            assert!(
                matches!(check_path(bad, &dirs), Err(EnvFileError::Invalid(_))),
                "{bad:?}"
            );
        }
    }

    struct Fixture {
        _tmp: tempfile::TempDir,
        allowed: PathBuf,
        outside: PathBuf,
    }

    fn fixture() -> Fixture {
        let tmp = tempfile::TempDir::new().unwrap();
        let allowed = tmp.path().join("allowed");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(allowed.join("app")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        Fixture {
            _tmp: tmp,
            allowed,
            outside,
        }
    }

    fn s(p: &Path) -> String {
        p.to_string_lossy().to_string()
    }

    #[test]
    fn reads_allowed_files() {
        let f = fixture();
        let file = f.allowed.join("app").join(".env");
        std::fs::write(&file, "A=1\nB='two'\n").unwrap();
        let v = read_env_file(&s(&file), std::slice::from_ref(&f.allowed)).unwrap();
        assert_eq!(v.get("A"), Some("1"));
        assert_eq!(v.get("B"), Some("two"));
        assert!(validate_setting(&s(&file), std::slice::from_ref(&f.allowed)).is_ok());
    }

    #[test]
    fn refuses_files_outside_the_allowed_directories() {
        let f = fixture();
        let file = f.outside.join(".env");
        std::fs::write(&file, "A=1").unwrap();
        let allowed = std::slice::from_ref(&f.allowed);
        let err = read_env_file(&s(&file), allowed).unwrap_err();
        assert!(matches!(err, EnvFileError::OutsideAllowed { .. }), "{err}");
        assert!(validate_setting(&s(&file), allowed).is_err());
        // Missing, in a directory that isn't allowed either.
        let missing = f.outside.join("missing.env");
        assert!(matches!(
            read_env_file(&s(&missing), allowed),
            Err(EnvFileError::OutsideAllowed { .. })
        ));
        // `..` is refused before anything is resolved.
        let sneaky = format!("{}/app/../../outside/.env", s(&f.allowed));
        assert!(matches!(
            read_env_file(&sneaky, allowed),
            Err(EnvFileError::Invalid(_))
        ));
        // The allowed directory itself is not a file inside it.
        assert!(matches!(
            read_env_file(&s(&f.allowed), allowed),
            Err(EnvFileError::OutsideAllowed { .. })
        ));
    }

    #[test]
    fn missing_directories_and_files() {
        let f = fixture();
        let allowed = std::slice::from_ref(&f.allowed);
        let missing = f.allowed.join("app").join("missing.env");
        let err = read_env_file(&s(&missing), allowed).unwrap_err();
        assert!(matches!(err, EnvFileError::NotFound(_)), "{err}");
        // Saving the setting is fine; the file can be created afterwards.
        assert!(validate_setting(&s(&missing), allowed).is_ok());
        // An allowed directory that doesn't exist allows nothing.
        let gone = [f.allowed.join("nope")];
        assert!(read_env_file(&s(&missing), &gone).is_err());
    }

    #[test]
    fn refuses_directories_and_big_files() {
        let f = fixture();
        let allowed = std::slice::from_ref(&f.allowed);
        let dir = f.allowed.join("app");
        assert!(matches!(
            read_env_file(&s(&dir), allowed),
            Err(EnvFileError::NotRegular(_))
        ));
        let big = f.allowed.join("big.env");
        let line = "A=xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\n";
        std::fs::write(
            &big,
            line.repeat((MAX_ENV_FILE_BYTES as usize / line.len()) + 1),
        )
        .unwrap();
        assert!(matches!(
            read_env_file(&s(&big), allowed),
            Err(EnvFileError::TooBig(_))
        ));
        let binary = f.allowed.join("binary.env");
        std::fs::write(&binary, [b'A', b'=', 0xff, 0xfe]).unwrap();
        assert!(matches!(
            read_env_file(&s(&binary), allowed),
            Err(EnvFileError::NotUtf8(_))
        ));
    }

    #[test]
    fn parse_errors_name_the_file_and_line_only() {
        let f = fixture();
        let file = f.allowed.join("bad.env");
        std::fs::write(&file, "A=1\nB=\"s3cret-unterminated\n").unwrap();
        let err = read_env_file(&s(&file), std::slice::from_ref(&f.allowed)).unwrap_err();
        let shown = err.to_string();
        assert!(shown.contains("line 2"), "{shown}");
        assert!(shown.contains("bad.env"), "{shown}");
        assert!(!shown.contains("s3cret"), "{shown}");
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_resolved_before_the_check() {
        let f = fixture();
        let allowed = std::slice::from_ref(&f.allowed);
        let target = f.outside.join("secret.env");
        std::fs::write(&target, "A=1").unwrap();
        // A link inside the allowed directory pointing outside: refused.
        let link = f.allowed.join("app").join(".env");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(matches!(
            read_env_file(&s(&link), allowed),
            Err(EnvFileError::OutsideAllowed { .. })
        ));
        // A linked directory, too.
        let dir_link = f.allowed.join("linked");
        std::os::unix::fs::symlink(&f.outside, &dir_link).unwrap();
        assert!(matches!(
            read_env_file(&format!("{}/secret.env", s(&dir_link)), allowed),
            Err(EnvFileError::OutsideAllowed { .. })
        ));
        // A link that stays inside is fine.
        let real = f.allowed.join("app").join("real.env");
        std::fs::write(&real, "B=2").unwrap();
        let inner_link = f.allowed.join("inner.env");
        std::os::unix::fs::symlink(&real, &inner_link).unwrap();
        assert_eq!(
            read_env_file(&s(&inner_link), allowed).unwrap().get("B"),
            Some("2")
        );
        // An allowed directory that is itself a link is resolved too.
        let allowed_link = f.outside.join("allowed-link");
        std::os::unix::fs::symlink(&f.allowed, &allowed_link).unwrap();
        assert!(read_env_file(&s(&real), &[allowed_link]).is_ok());
        // FIFOs and other special files are refused.
        let fifo = f.allowed.join("fifo.env");
        let status = std::process::Command::new("mkfifo").arg(&fifo).status();
        if status.is_ok_and(|s| s.success()) {
            assert!(matches!(
                read_env_file(&s(&fifo), allowed),
                Err(EnvFileError::NotRegular(_))
            ));
        }
    }

    #[cfg(unix)]
    #[test]
    fn unreadable_files_explain_the_fix() {
        use std::os::unix::fs::PermissionsExt;
        let f = fixture();
        let file = f.allowed.join("root-only.env");
        std::fs::write(&file, "A=1").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o000)).unwrap();
        // Root reads anything; nothing to check then.
        if std::fs::read(&file).is_ok() {
            return;
        }
        let err = read_env_file(&s(&file), std::slice::from_ref(&f.allowed)).unwrap_err();
        assert!(
            matches!(err, EnvFileError::PermissionDenied { .. }),
            "{err}"
        );
        let shown = err.to_string();
        assert!(shown.contains("permission denied"), "{shown}");
        assert!(shown.contains("chmod 640"), "{shown}");
        assert!(shown.contains(&s(&file)), "{shown}");
    }
}
