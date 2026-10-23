//! Start commands that hard-code a port.
//!
//! Aegis tells an app which port to use through `PORT` (and gives every
//! release its own port behind the built-in proxy), so a start script such as
//! `next start -p 4000` must become `npx next start -p $PORT`. This module
//! finds such scripts in `package.json` and proposes the `$PORT` form.

use std::path::Path;

/// A Node.js start script that fixes its port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartHint {
    /// The port the script hard-codes.
    pub port: u16,
    /// The `start` script as written in package.json.
    pub script: String,
    /// The same command reading the port from `$PORT`.
    pub suggested_start: String,
}

/// Commands that run a script rather than a package's binary; anything
/// else at the start of a script is a binary from `node_modules/.bin`, which
/// `npm start` finds on its own but a plain shell needs `npx` for.
const RUNNERS: &[&str] = &[
    "node", "npm", "npx", "yarn", "pnpm", "bun", "deno", "sh", "bash", "exec", "env", "python",
    "python3",
];

fn parse_port(text: &str) -> Option<u16> {
    text.parse::<u16>().ok().filter(|p| *p > 0)
}

/// Finds a hard-coded port in a command (`-p 4000`, `--port 4000`,
/// `--port=4000`, `PORT=4000 ...`) and returns it with the command rewritten
/// to use `$PORT`.
pub fn hardcoded_port(command: &str) -> Option<(u16, String)> {
    let tokens: Vec<&str> = command.split_whitespace().collect();
    let mut out: Vec<String> = Vec::with_capacity(tokens.len());
    let mut found = None;
    let mut i = 0;
    while i < tokens.len() {
        let t = tokens[i];
        if found.is_none() {
            if matches!(t, "-p" | "--port" | "-P") {
                if let Some(port) = tokens.get(i + 1).and_then(|n| parse_port(n)) {
                    found = Some(port);
                    out.push(t.to_string());
                    out.push("$PORT".to_string());
                    i += 2;
                    continue;
                }
            }
            if let Some(port) = t.strip_prefix("--port=").and_then(parse_port) {
                found = Some(port);
                out.push("--port=$PORT".to_string());
                i += 1;
                continue;
            }
            if let Some(port) = t.strip_prefix("PORT=").and_then(parse_port) {
                // Aegis sets PORT itself.
                found = Some(port);
                i += 1;
                continue;
            }
        }
        out.push(t.to_string());
        i += 1;
    }
    let port = found?;
    if out.is_empty() {
        return None;
    }
    let first = out[0].as_str();
    let bare_binary = !RUNNERS.contains(&first)
        && !first.contains('/')
        && !first.contains('=')
        && first
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.@".contains(c));
    let mut suggested = out.join(" ");
    if bare_binary {
        suggested = format!("npx {}", suggested);
    }
    Some((port, suggested))
}

/// The `scripts.start` of a directory's package.json, if any.
pub fn package_start_script(dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(dir.join("package.json")).ok()?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;
    json.get("scripts")?
        .get("start")?
        .as_str()
        .map(str::to_string)
}

/// A hint for a Node.js app whose start script hard-codes its port.
pub fn node_start_hint(dir: &Path) -> Option<StartHint> {
    let script = package_start_script(dir)?;
    let (port, suggested_start) = hardcoded_port(&script)?;
    Some(StartHint {
        port,
        script,
        suggested_start,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hardcoded_ports_are_rewritten() {
        let h = |c: &str| hardcoded_port(c);
        assert_eq!(
            h("next start -p 4000"),
            Some((4000, "npx next start -p $PORT".into()))
        );
        assert_eq!(
            h("next start --port 3001"),
            Some((3001, "npx next start --port $PORT".into()))
        );
        assert_eq!(
            h("nuxt start --port=8080"),
            Some((8080, "npx nuxt start --port=$PORT".into()))
        );
        assert_eq!(
            h("PORT=4000 node server.js"),
            Some((4000, "node server.js".into()))
        );
        assert_eq!(
            h("node dist/main.js --port 5000"),
            Some((5000, "node dist/main.js --port $PORT".into()))
        );
        assert_eq!(
            h("./node_modules/.bin/serve -p 5000 build"),
            Some((5000, "./node_modules/.bin/serve -p $PORT build".into()))
        );
        // Not ports.
        assert_eq!(h("next start"), None);
        assert_eq!(h("node -p 'process.version'"), None);
        assert_eq!(h("serve -p $PORT"), None);
        assert_eq!(h("PORT=0 node x"), None);
        assert_eq!(h(""), None);
    }

    #[test]
    fn package_json_start_script() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"name":"img-man","scripts":{"build":"next build","start":"next start -p 4000"}}"#,
        )
        .unwrap();
        let hint = node_start_hint(dir.path()).unwrap();
        assert_eq!(hint.port, 4000);
        assert_eq!(hint.script, "next start -p 4000");
        assert_eq!(hint.suggested_start, "npx next start -p $PORT");
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"scripts":{"start":"node a.js"}}"#,
        )
        .unwrap();
        assert_eq!(node_start_hint(dir.path()), None);
        std::fs::write(dir.path().join("package.json"), "not json").unwrap();
        assert_eq!(node_start_hint(dir.path()), None);
    }
}
