//! `aegis migrate`: turns a PM2 ecosystem file or a systemd unit into an
//! aegis.toml (name, start command, port, environment, restart policy).

use aegis_config::{BuildSection, DeploySection, ProjectFile, ProjectSection};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::path::Path;

/// The generated project file and what could not be carried over.
#[derive(Debug)]
pub struct Migration {
    pub file: ProjectFile,
    pub warnings: Vec<String>,
    /// e.g. "PM2 app 'api' in ecosystem.config.js"
    pub source: String,
}

impl Migration {
    /// The aegis.toml text.
    pub fn render(&self) -> anyhow::Result<String> {
        Ok(format!(
            "# Migrated from {} by 'aegis migrate'.\n# Review it, then run 'aegis validate' and 'aegis deploy'.\n{}",
            self.source,
            toml::to_string_pretty(&self.file)?
        ))
    }
}

/// Quotes a word for `sh` when needed. `$` is left alone so variables still
/// expand, as they would under systemd.
pub fn quote(word: &str) -> String {
    let safe = |c: char| c.is_ascii_alphanumeric() || "-_./=:@%+,$".contains(c);
    if !word.is_empty() && word.chars().all(safe) {
        return word.to_string();
    }
    let mut out = String::from("\"");
    for c in word.chars() {
        if matches!(c, '"' | '\\' | '`') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// Splits a command line or `Environment=` value into words, honouring
/// single quotes, double quotes and backslash escapes.
pub fn split_words(s: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut chars = s.chars();
    let (mut single, mut double) = (false, false);
    while let Some(c) = chars.next() {
        match c {
            '\'' if !double => {
                single = !single;
                in_word = true;
            }
            '"' if !single => {
                double = !double;
                in_word = true;
            }
            '\\' if !single => {
                if let Some(next) = chars.next() {
                    word.push(next);
                }
                in_word = true;
            }
            c if c.is_whitespace() && !single && !double => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            c => {
                word.push(c);
                in_word = true;
            }
        }
    }
    if in_word {
        words.push(word);
    }
    words
}

/// Moves a numeric `PORT` from the environment to `deploy.port` (Aegis
/// exports `deploy.port` as `PORT`).
fn take_port(env: &mut BTreeMap<String, String>, warnings: &mut Vec<String>) -> Option<u16> {
    let value = env.get("PORT")?;
    match value.trim().parse::<u16>() {
        Ok(port) if port > 0 => {
            env.remove("PORT");
            Some(port)
        }
        _ => {
            warnings.push(format!(
                "PORT='{}' is not a port number; kept it in [env] and did not set deploy.port.",
                value
            ));
            None
        }
    }
}

fn project_file(
    name: String,
    start_command: String,
    mut env: BTreeMap<String, String>,
    restart_policy: Option<String>,
    warnings: &mut Vec<String>,
) -> ProjectFile {
    let port = take_port(&mut env, warnings);
    ProjectFile {
        project: ProjectSection {
            id: Some(aegis_types::ProjectId::new().to_string()),
            name: Some(name),
            ..Default::default()
        },
        build: BuildSection {
            start_command: Some(start_command),
            ..Default::default()
        },
        deploy: DeploySection {
            port,
            restart_policy,
            ..Default::default()
        },
        env,
    }
}

// ----------------------------------------------------------------------
// PM2
// ----------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Punct(char),
    Str(String),
    Num(String),
    Ident(String),
    Other(char),
}

/// Tokenizes enough JavaScript to read object and array literals.
fn lex(src: &str) -> anyhow::Result<Vec<Tok>> {
    let chars: Vec<char> = src.chars().collect();
    let mut toks = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        if c.is_whitespace() {
            i += 1;
        } else if c == '/' && next == Some('/') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
        } else if c == '/' && next == Some('*') {
            i += 2;
            while i < chars.len() && !(chars[i] == '*' && chars.get(i + 1) == Some(&'/')) {
                i += 1;
            }
            i += 2;
        } else if matches!(c, '"' | '\'' | '`') {
            let mut s = String::new();
            i += 1;
            loop {
                let Some(&ch) = chars.get(i) else {
                    anyhow::bail!("unterminated string literal");
                };
                i += 1;
                if ch == c {
                    break;
                }
                if ch == '\\' {
                    let Some(&esc) = chars.get(i) else {
                        anyhow::bail!("unterminated string literal");
                    };
                    i += 1;
                    match esc {
                        'n' => s.push('\n'),
                        't' => s.push('\t'),
                        'r' => s.push('\r'),
                        '0' => s.push('\0'),
                        'u' => {
                            let hex: String = chars.iter().skip(i).take(4).collect();
                            if let Some(ch) =
                                u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32)
                            {
                                s.push(ch);
                                i += 4;
                            } else {
                                s.push('u');
                            }
                        }
                        '\n' => {}
                        other => s.push(other),
                    }
                } else {
                    s.push(ch);
                }
            }
            toks.push(Tok::Str(s));
        } else if c.is_ascii_digit() || (c == '.' && next.is_some_and(|n| n.is_ascii_digit())) {
            let start = i;
            while i < chars.len()
                && (chars[i].is_ascii_alphanumeric()
                    || chars[i] == '.'
                    || chars[i] == '_'
                    || (matches!(chars[i], '+' | '-') && matches!(chars[i - 1], 'e' | 'E')))
            {
                i += 1;
            }
            toks.push(Tok::Num(chars[start..i].iter().collect()));
        } else if c.is_alphabetic() || c == '_' || c == '$' {
            let start = i;
            while i < chars.len()
                && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == '$')
            {
                i += 1;
            }
            toks.push(Tok::Ident(chars[start..i].iter().collect()));
        } else if matches!(c, '{' | '}' | '[' | ']' | ':' | ',' | ';') {
            toks.push(Tok::Punct(c));
            i += 1;
        } else {
            toks.push(Tok::Other(c));
            i += 1;
        }
    }
    Ok(toks)
}

/// Reads JavaScript literals into JSON values. Anything that isn't a plain
/// literal (`process.env.X`, function calls, spreads) becomes `null` with a
/// warning.
struct Parser<'a> {
    toks: &'a [Tok],
    pos: usize,
    warnings: Vec<String>,
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn at_end_of_value(&self) -> bool {
        matches!(self.peek(), None | Some(Tok::Punct(',' | '}' | ']' | ';')))
    }

    /// Skips the rest of an expression, up to the next `,`, `}`, `]` or `;`
    /// at this nesting level.
    fn skip_expr(&mut self) {
        let mut depth = 0usize;
        while let Some(tok) = self.peek() {
            match tok {
                Tok::Punct('{' | '[') | Tok::Other('(') => depth += 1,
                Tok::Punct('}' | ']') | Tok::Other(')') => {
                    if depth == 0 {
                        return;
                    }
                    depth -= 1;
                }
                Tok::Punct(',' | ';') if depth == 0 => return,
                _ => {}
            }
            self.pos += 1;
        }
    }

    fn unsupported(&mut self, path: &str) -> Value {
        self.skip_expr();
        self.warnings.push(format!(
            "{}: not a plain value (e.g. process.env or a function call); ignored.",
            path
        ));
        Value::Null
    }

    fn value(&mut self, path: &str) -> anyhow::Result<Value> {
        let start = self.pos;
        let value = match self.peek().cloned() {
            None => anyhow::bail!("unexpected end of file"),
            Some(Tok::Punct('{')) => return self.object(path),
            Some(Tok::Punct('[')) => return self.array(path),
            Some(Tok::Str(s)) => {
                self.pos += 1;
                Value::String(s)
            }
            Some(Tok::Num(n)) => {
                self.pos += 1;
                number(&n)
            }
            Some(Tok::Other('-')) => match self.toks.get(self.pos + 1) {
                Some(Tok::Num(n)) => {
                    self.pos += 2;
                    number(&format!("-{}", n))
                }
                _ => return Ok(self.unsupported(path)),
            },
            Some(Tok::Ident(id)) => {
                self.pos += 1;
                match id.as_str() {
                    "true" => Value::Bool(true),
                    "false" => Value::Bool(false),
                    "null" | "undefined" => Value::Null,
                    _ => {
                        self.pos = start;
                        return Ok(self.unsupported(path));
                    }
                }
            }
            Some(_) => return Ok(self.unsupported(path)),
        };
        if self.at_end_of_value() {
            Ok(value)
        } else {
            // e.g. `3000 || x` or `"a" + b`
            self.pos = start;
            Ok(self.unsupported(path))
        }
    }

    fn object(&mut self, path: &str) -> anyhow::Result<Value> {
        self.pos += 1; // {
        let mut map = Map::new();
        loop {
            let key = match self.peek().cloned() {
                None => anyhow::bail!("unterminated object at {}", path),
                Some(Tok::Punct('}')) => {
                    self.pos += 1;
                    return Ok(Value::Object(map));
                }
                Some(Tok::Punct(',')) => {
                    self.pos += 1;
                    continue;
                }
                Some(Tok::Ident(k) | Tok::Str(k) | Tok::Num(k)) => {
                    self.pos += 1;
                    k
                }
                Some(_) => {
                    self.unsupported(&format!("{}.<entry>", path));
                    continue;
                }
            };
            let key_path = format!("{}.{}", path, key);
            if self.peek() == Some(&Tok::Punct(':')) {
                self.pos += 1;
                let value = self.value(&key_path)?;
                map.insert(key, value);
            } else {
                // Shorthand `{ env }` or a method.
                self.unsupported(&key_path);
            }
        }
    }

    fn array(&mut self, path: &str) -> anyhow::Result<Value> {
        self.pos += 1; // [
        let mut items = Vec::new();
        loop {
            match self.peek() {
                None => anyhow::bail!("unterminated array at {}", path),
                Some(Tok::Punct(']')) => {
                    self.pos += 1;
                    return Ok(Value::Array(items));
                }
                Some(Tok::Punct(',')) => self.pos += 1,
                _ => {
                    let item = self.value(&format!("{}[{}]", path, items.len()))?;
                    items.push(item);
                }
            }
        }
    }
}

fn number(text: &str) -> Value {
    if let Ok(i) = text.parse::<i64>() {
        return Value::from(i);
    }
    text.parse::<f64>()
        .ok()
        .and_then(serde_json::Number::from_f64)
        .map(Value::Number)
        .unwrap_or_else(|| Value::String(text.to_string()))
}

/// Finds the `apps: [...]` list in an ecosystem.config.js.
fn js_apps(src: &str) -> anyhow::Result<(Value, Vec<String>)> {
    let toks = lex(src)?;
    let start = toks
        .windows(2)
        .position(|w| {
            matches!(&w[0], Tok::Ident(k) | Tok::Str(k) if k == "apps") && w[1] == Tok::Punct(':')
        })
        .ok_or_else(|| anyhow::anyhow!("no 'apps: [...]' list found"))?;
    let mut parser = Parser {
        toks: &toks,
        pos: start + 2,
        warnings: Vec::new(),
    };
    let apps = parser.value("apps")?;
    Ok((apps, parser.warnings))
}

/// The app definitions in a PM2 ecosystem file (JSON, or JavaScript on a
/// best-effort basis), plus warnings about values that could not be read.
pub fn pm2_apps(file_name: &str, text: &str) -> anyhow::Result<(Vec<Value>, Vec<String>)> {
    let parsed = if file_name.ends_with(".json") {
        match serde_json::from_str::<Value>(text) {
            Ok(v) => Ok((v, Vec::new())),
            // Tolerate comments and trailing commas.
            Err(e) => js_apps(text).map_err(|_| anyhow::anyhow!("invalid JSON: {}", e)),
        }
    } else {
        js_apps(text)
    };
    let (value, warnings) = parsed.map_err(|e| {
        anyhow::anyhow!(
            "Could not read PM2 apps from {}: {}. Export it as JSON with 'pm2 ecosystem' or write {{\"apps\": [...]}}.",
            file_name,
            e
        )
    })?;
    let apps = match value {
        Value::Object(mut obj) if obj.contains_key("apps") => {
            obj.remove("apps").unwrap_or_default()
        }
        single @ Value::Object(_) => Value::Array(vec![single]),
        other => other,
    };
    match apps {
        Value::Array(apps) if !apps.is_empty() => Ok((apps, warnings)),
        Value::Object(_) => Ok((vec![apps], warnings)),
        _ => anyhow::bail!("No apps found in {}", file_name),
    }
}

fn app_name(app: &Value) -> Option<&str> {
    app.get("name").and_then(|v| v.as_str())
}

/// The interpreter PM2 picks from a script's extension.
fn default_interpreter(script: &str) -> Option<&'static str> {
    let ext = Path::new(script).extension()?.to_str()?;
    Some(match ext {
        "js" | "cjs" | "mjs" => "node",
        "py" => "python3",
        "sh" => "bash",
        "rb" => "ruby",
        "php" => "php",
        "pl" => "perl",
        "coffee" => "coffee",
        _ => return None,
    })
}

/// A string, or an array of strings joined as shell words.
fn words(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::String(s) if !s.trim().is_empty() => Some(s.trim().to_string()),
        Value::Array(items) => {
            let parts: Vec<String> = items
                .iter()
                .filter_map(|i| match i {
                    Value::String(s) => Some(quote(s)),
                    Value::Number(n) => Some(n.to_string()),
                    _ => None,
                })
                .collect();
            (!parts.is_empty()).then(|| parts.join(" "))
        }
        _ => None,
    }
}

/// Builds an aegis.toml from one app of a PM2 ecosystem file. With several
/// apps, `pick` must name one.
pub fn from_pm2(file_name: &str, text: &str, pick: Option<&str>) -> anyhow::Result<Migration> {
    let (apps, mut warnings) = pm2_apps(file_name, text)?;
    let app = match pick {
        Some(name) => apps
            .iter()
            .find(|a| app_name(a) == Some(name))
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "No app named '{}' in {}. Apps: {}",
                    name,
                    file_name,
                    names(&apps)
                )
            })?,
        None if apps.len() == 1 => &apps[0],
        None => anyhow::bail!(
            "{} defines {} apps; choose one with --app NAME. Apps: {}",
            file_name,
            apps.len(),
            names(&apps)
        ),
    };
    let name = app_name(app).unwrap_or("app").to_string();
    let script = app
        .get("script")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("PM2 app '{}' has no script", name))?;

    let interpreter = match app.get("interpreter").and_then(|v| v.as_str()) {
        Some("none") | Some("") => None,
        Some(i) => Some(i.to_string()),
        None => default_interpreter(script).map(str::to_string),
    };
    let mut command = Vec::new();
    if let Some(interp) = &interpreter {
        command.push(quote(interp));
        let interp_args = words(app.get("interpreter_args")).or_else(|| {
            (interp == "node")
                .then(|| words(app.get("node_args")))
                .flatten()
        });
        command.extend(interp_args);
    }
    command.push(quote(script));
    command.extend(words(app.get("args")));
    let start_command = command.join(" ");

    let mut env = BTreeMap::new();
    match app.get("env") {
        Some(Value::Object(vars)) => {
            for (k, v) in vars {
                match v {
                    Value::String(s) => {
                        env.insert(k.clone(), s.clone());
                    }
                    Value::Number(n) => {
                        env.insert(k.clone(), n.to_string());
                    }
                    Value::Bool(b) => {
                        env.insert(k.clone(), b.to_string());
                    }
                    Value::Null => {}
                    _ => warnings.push(format!("env.{} is not a plain value; ignored.", k)),
                }
            }
        }
        Some(Value::Null) | None => {}
        Some(_) => warnings.push("env is not an object; ignored.".to_string()),
    }

    let instances = app.get("instances");
    let many = match instances {
        Some(Value::Number(n)) => n.as_i64() != Some(1),
        Some(Value::String(s)) => s != "1",
        _ => false,
    };
    if many {
        warnings.push(format!(
            "instances={} ignored: Aegis runs one process per project.",
            instances.map(|v| v.to_string()).unwrap_or_default()
        ));
    }
    if app.get("exec_mode").and_then(|v| v.as_str()) == Some("cluster") {
        warnings.push("exec_mode=cluster ignored: the app runs as a single process.".to_string());
    }
    if let Some(cwd) = app.get("cwd").and_then(|v| v.as_str()) {
        if cwd != "." && cwd != "./" {
            warnings.push(format!(
                "cwd '{}' is not used: Aegis runs the app from its release directory. Put aegis.toml in {} and deploy from there.",
                cwd, cwd
            ));
        }
    }
    if let Value::Object(obj) = app {
        for key in obj.keys() {
            if key.starts_with("env_") {
                warnings.push(format!(
                    "{} not imported (only env is); copy the values you need into [env].",
                    key
                ));
            }
        }
    }
    for (key, hint) in [
        ("watch", "redeploy instead of watching files"),
        (
            "max_memory_restart",
            "set a memory limit in the dashboard (Resources)",
        ),
        ("cron_restart", "use 'aegis schedule' for timed deployments"),
        (
            "log_file",
            "logs are kept by Aegis; read them with 'aegis logs'",
        ),
        (
            "out_file",
            "logs are kept by Aegis; read them with 'aegis logs'",
        ),
        (
            "error_file",
            "logs are kept by Aegis; read them with 'aegis logs'",
        ),
    ] {
        if app
            .get(key)
            .is_some_and(|v| !v.is_null() && *v != Value::Bool(false))
        {
            warnings.push(format!("{} ignored: {}.", key, hint));
        }
    }
    let restart_policy =
        (app.get("autorestart") == Some(&Value::Bool(false))).then(|| "never".to_string());

    let file = project_file(
        name.clone(),
        start_command,
        env,
        restart_policy,
        &mut warnings,
    );
    Ok(Migration {
        file,
        warnings,
        source: format!("PM2 app '{}' in {}", name, file_name),
    })
}

fn names(apps: &[Value]) -> String {
    apps.iter()
        .map(|a| app_name(a).unwrap_or("(unnamed)"))
        .collect::<Vec<_>>()
        .join(", ")
}

// ----------------------------------------------------------------------
// systemd
// ----------------------------------------------------------------------

/// Parses a systemd EnvironmentFile (`KEY=value` lines).
pub fn parse_env_file(text: &str) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        let value = match split_words(value).as_slice() {
            [single] if value.starts_with(['"', '\'']) => single.clone(),
            _ => value.to_string(),
        };
        env.insert(key.trim().to_string(), value);
    }
    env
}

/// Builds an aegis.toml from a systemd service unit. `read` loads
/// EnvironmentFile= paths (returns `None` when missing).
pub fn from_systemd(
    path: &Path,
    text: &str,
    read: &dyn Fn(&Path) -> Option<String>,
) -> anyhow::Result<Migration> {
    let file_name = path.display().to_string();
    let mut warnings = Vec::new();

    // Join `\` continuations into logical lines.
    let mut lines = Vec::new();
    let mut current = String::new();
    for raw in text.lines() {
        let trimmed = raw.trim();
        if current.is_empty() && (trimmed.starts_with('#') || trimmed.starts_with(';')) {
            continue;
        }
        match trimmed.strip_suffix('\\') {
            Some(part) => {
                current.push_str(part);
                current.push(' ');
            }
            None => {
                current.push_str(trimmed);
                lines.push(std::mem::take(&mut current));
            }
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }

    let mut section = String::new();
    let mut exec_start: Vec<String> = Vec::new();
    let mut working_dir = None;
    let mut environment: Vec<String> = Vec::new();
    let mut env_files: Vec<String> = Vec::new();
    let mut restart = None;
    for line in lines {
        let line = line.trim();
        if line.starts_with('[') && line.ends_with(']') {
            section = line[1..line.len() - 1].to_string();
            continue;
        }
        if section != "Service" {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim().to_string();
        match key.trim() {
            // An empty assignment resets the list.
            "ExecStart" if value.is_empty() => exec_start.clear(),
            "ExecStart" => exec_start.push(value),
            "WorkingDirectory" => working_dir = Some(value),
            "Environment" if value.is_empty() => environment.clear(),
            "Environment" => environment.push(value),
            "EnvironmentFile" if value.is_empty() => env_files.clear(),
            "EnvironmentFile" => env_files.push(value),
            "Restart" => restart = Some(value),
            "User" | "Group" => warnings.push(format!(
                "{}={} ignored: the app runs as the daemon's user.",
                key.trim(),
                value
            )),
            "ExecStartPre" | "ExecStartPost" | "ExecReload" | "ExecStop" => warnings.push(format!(
                "{} not imported; use build.install_command/build_command for setup steps.",
                key.trim()
            )),
            _ => {}
        }
    }

    let Some(first) = exec_start.first() else {
        anyhow::bail!("No ExecStart= in the [Service] section of {}", file_name);
    };
    if exec_start.len() > 1 {
        warnings.push(format!(
            "{} ExecStart= lines found; only the first is used.",
            exec_start.len()
        ));
    }
    let mut exec = first.as_str();
    let mut argv0_given = false;
    while let Some(c) = exec.chars().next().filter(|c| "-@:+!".contains(*c)) {
        argv0_given |= c == '@';
        exec = &exec[1..];
    }
    let mut argv = split_words(exec);
    if argv0_given && argv.len() > 1 {
        // `@/usr/bin/app name args`: the second word is argv[0], not an argument.
        argv.remove(1);
    }
    if argv.is_empty() {
        anyhow::bail!("ExecStart= in {} has no command", file_name);
    }
    let start_command = argv.iter().map(|w| quote(w)).collect::<Vec<_>>().join(" ");
    if start_command.contains('%') {
        warnings.push(
            "ExecStart uses % specifiers (e.g. %i), which only systemd expands; replace them."
                .to_string(),
        );
    }

    let mut env = BTreeMap::new();
    for assignments in &environment {
        for word in split_words(assignments) {
            match word.split_once('=') {
                Some((k, v)) => {
                    env.insert(k.to_string(), v.to_string());
                }
                None => warnings.push(format!("Environment entry '{}' has no '='; ignored.", word)),
            }
        }
    }
    // Like systemd, files override Environment=.
    for entry in &env_files {
        let (optional, name) = match entry.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, entry.as_str()),
        };
        match read(Path::new(name)) {
            Some(text) => env.extend(parse_env_file(&text)),
            None if optional => {}
            None => warnings.push(format!(
                "EnvironmentFile {} could not be read; its variables were not imported.",
                name
            )),
        }
    }

    if let Some(dir) = working_dir {
        let dir = dir.trim_start_matches('-');
        warnings.push(format!(
            "WorkingDirectory={} is not used: Aegis runs the app from its release directory. Put aegis.toml in {} and deploy from there.",
            dir, dir
        ));
    }
    let restart_policy = restart.and_then(|r| match r.as_str() {
        "always" => Some("always".to_string()),
        "on-failure" => Some("on-failure".to_string()),
        "no" => Some("never".to_string()),
        "on-abnormal" | "on-abort" | "on-watchdog" => {
            warnings.push(format!(
                "Restart={} mapped to restart_policy = \"on-failure\".",
                r
            ));
            Some("on-failure".to_string())
        }
        "on-success" => {
            warnings.push(
                "Restart=on-success has no equivalent; using restart_policy = \"never\"."
                    .to_string(),
            );
            Some("never".to_string())
        }
        other => {
            warnings.push(format!(
                "Restart={} not recognised; using the default (always).",
                other
            ));
            None
        }
    });

    let stem = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("app")
        .trim_end_matches(".service");
    let name = stem.trim_end_matches('@').to_string();
    let file = project_file(name, start_command, env, restart_policy, &mut warnings);
    Ok(Migration {
        file,
        warnings,
        source: format!("systemd unit {}", file_name),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_and_splits_words() {
        assert_eq!(quote("node"), "node");
        assert_eq!(quote("$HOME/app"), "$HOME/app");
        assert_eq!(quote("two words"), "\"two words\"");
        assert_eq!(quote(r#"{"a":1}"#), r#""{\"a\":1}""#);
        assert_eq!(quote(""), "\"\"");
        assert_eq!(
            split_words(r#"a "b c" 'd e' f\ g "h\"i""#),
            vec!["a", "b c", "d e", "f g", "h\"i"]
        );
        assert_eq!(split_words("  "), Vec::<String>::new());
        assert_eq!(split_words("''"), vec![""]);
    }

    const ECOSYSTEM_JS: &str = r#"
// PM2 config
module.exports = {
  apps : [{
    name   : "api",
    script : "./server.js",
    args: ["--color", "a b"],
    node_args: '--max-old-space-size=512',
    cwd: "/srv/api",
    instances: 4,
    exec_mode: "cluster",
    env: {
      NODE_ENV: 'production',
      PORT: 3000,
      SECRET: process.env.SECRET,
      DEBUG: false,
    },
    env_staging: { NODE_ENV: "staging" },
    /* trailing comma below */
  }, {
    name: 'worker',
    script: 'worker.py',
    interpreter: '/usr/bin/python3.11',
    autorestart: false,
    max_memory_restart: '300M',
    restart_delay: -1,
  }],
  deploy: { production: { user: "x" } }
};
"#;

    #[test]
    fn reads_pm2_javascript() {
        let (apps, warnings) = pm2_apps("ecosystem.config.js", ECOSYSTEM_JS).unwrap();
        assert_eq!(apps.len(), 2);
        assert_eq!(apps[1]["restart_delay"], Value::from(-1));
        assert!(
            warnings.iter().any(|w| w.contains("apps[0].env.SECRET")),
            "{:?}",
            warnings
        );

        let err = from_pm2("ecosystem.config.js", ECOSYSTEM_JS, None).unwrap_err();
        assert!(
            err.to_string().contains("--app NAME. Apps: api, worker"),
            "{}",
            err
        );
        let err = from_pm2("ecosystem.config.js", ECOSYSTEM_JS, Some("web")).unwrap_err();
        assert!(err.to_string().contains("No app named 'web'"), "{}", err);

        let m = from_pm2("ecosystem.config.js", ECOSYSTEM_JS, Some("api")).unwrap();
        let f = &m.file;
        assert_eq!(f.project.name.as_deref(), Some("api"));
        assert_eq!(
            f.build.start_command.as_deref(),
            Some("node --max-old-space-size=512 ./server.js --color \"a b\"")
        );
        assert_eq!(f.deploy.port, Some(3000));
        assert_eq!(
            f.env.get("NODE_ENV").map(String::as_str),
            Some("production")
        );
        assert_eq!(f.env.get("DEBUG").map(String::as_str), Some("false"));
        assert!(!f.env.contains_key("PORT") && !f.env.contains_key("SECRET"));
        assert!(f.project.id.is_some());
        let all = m.warnings.join("\n");
        for expected in [
            "instances=4",
            "exec_mode=cluster",
            "cwd '/srv/api'",
            "env_staging",
        ] {
            assert!(all.contains(expected), "missing {}: {}", expected, all);
        }

        let m = from_pm2("ecosystem.config.js", ECOSYSTEM_JS, Some("worker")).unwrap();
        assert_eq!(
            m.file.build.start_command.as_deref(),
            Some("/usr/bin/python3.11 worker.py")
        );
        assert_eq!(m.file.deploy.restart_policy.as_deref(), Some("never"));
        assert!(m
            .warnings
            .iter()
            .any(|w| w.starts_with("max_memory_restart")));
    }

    #[test]
    fn reads_pm2_json() {
        let json = r#"{"apps": [{"name": "web", "script": "npm", "args": "run start", "env": {"PORT": "8080"}}]}"#;
        let m = from_pm2("ecosystem.config.json", json, None).unwrap();
        assert_eq!(m.file.build.start_command.as_deref(), Some("npm run start"));
        assert_eq!(m.file.deploy.port, Some(8080));
        assert!(m.warnings.is_empty(), "{:?}", m.warnings);

        // A bare list, and a single app object.
        let m = from_pm2("apps.json", r#"[{"name": "a", "script": "a.sh"}]"#, None).unwrap();
        assert_eq!(m.file.build.start_command.as_deref(), Some("bash a.sh"));
        let m = from_pm2(
            "app.json",
            r#"{"script": "bin/app", "interpreter": "none"}"#,
            None,
        )
        .unwrap();
        assert_eq!(m.file.build.start_command.as_deref(), Some("bin/app"));
        assert_eq!(m.file.project.name.as_deref(), Some("app"));

        // JSON with comments falls back to the JavaScript reader.
        let m = from_pm2(
            "x.json",
            "{\"apps\": [{\"script\": \"a.js\",}], // c\n}",
            None,
        )
        .unwrap();
        assert_eq!(m.file.build.start_command.as_deref(), Some("node a.js"));

        assert!(from_pm2("x.json", r#"{"apps": []}"#, None).is_err());
        let err = from_pm2("x.json", r#"{"apps": [{"name": "a"}]}"#, None).unwrap_err();
        assert!(err.to_string().contains("has no script"), "{}", err);
        let err = from_pm2("x.js", "module.exports = config;", None).unwrap_err();
        assert!(
            err.to_string().contains("no 'apps: [...]' list found"),
            "{}",
            err
        );
        let m = from_pm2(
            "x.json",
            r#"{"apps": [{"script": "a", "env": {"PORT": "abc"}}]}"#,
            None,
        )
        .unwrap();
        assert_eq!(m.file.deploy.port, None);
        assert_eq!(m.file.env.get("PORT").map(String::as_str), Some("abc"));
    }

    #[test]
    fn renders_a_valid_aegis_toml() {
        let m = from_pm2(
            "e.json",
            r#"{"name": "web", "script": "a.js", "env": {"A": "1"}}"#,
            None,
        )
        .unwrap();
        let text = m.render().unwrap();
        assert!(text.starts_with("# Migrated from PM2 app 'web' in e.json"));
        let parsed = ProjectFile::parse(&text).unwrap();
        assert_eq!(parsed, m.file);
    }

    const UNIT: &str = r#"
[Unit]
Description=My API
ExecStart=/bin/false

[Service]
Type=simple
User=api
WorkingDirectory=-/srv/api
Environment="NODE_ENV=production" PORT=4000
Environment='GREETING=hello world'
EnvironmentFile=/etc/api.env
EnvironmentFile=-/etc/missing.env
# a comment
ExecStartPre=/usr/bin/npm ci
ExecStart=
ExecStart=-/usr/bin/node \
    /srv/api/server.js \
    --name "my api"
Restart=on-abnormal

[Install]
WantedBy=multi-user.target
"#;

    fn files(path: &Path) -> Option<String> {
        (path == Path::new("/etc/api.env")).then(|| {
            "# comment\nDB_URL=\"postgres://db/x\"\nexport TOKEN=abc\nPORT=4001\n".to_string()
        })
    }

    #[test]
    fn reads_systemd_units() {
        let m = from_systemd(
            Path::new("/etc/systemd/system/my-api.service"),
            UNIT,
            &files,
        )
        .unwrap();
        let f = &m.file;
        assert_eq!(f.project.name.as_deref(), Some("my-api"));
        assert_eq!(
            f.build.start_command.as_deref(),
            Some("/usr/bin/node /srv/api/server.js --name \"my api\"")
        );
        // EnvironmentFile overrides Environment=.
        assert_eq!(f.deploy.port, Some(4001));
        assert_eq!(
            f.env.get("NODE_ENV").map(String::as_str),
            Some("production")
        );
        assert_eq!(
            f.env.get("GREETING").map(String::as_str),
            Some("hello world")
        );
        assert_eq!(
            f.env.get("DB_URL").map(String::as_str),
            Some("postgres://db/x")
        );
        assert_eq!(f.env.get("TOKEN").map(String::as_str), Some("abc"));
        assert_eq!(f.deploy.restart_policy.as_deref(), Some("on-failure"));
        let all = m.warnings.join("\n");
        for expected in [
            "User=api",
            "ExecStartPre",
            "WorkingDirectory=/srv/api",
            "Restart=on-abnormal",
        ] {
            assert!(all.contains(expected), "missing {}: {}", expected, all);
        }
        assert!(!all.contains("missing.env"), "{}", all);
    }

    #[test]
    fn systemd_edge_cases() {
        let none = |_: &Path| None;
        let err =
            from_systemd(Path::new("a.service"), "[Service]\nType=simple\n", &none).unwrap_err();
        assert!(err.to_string().contains("No ExecStart="), "{}", err);
        // ExecStart outside [Service] doesn't count.
        assert!(from_systemd(Path::new("a.service"), "[Unit]\nExecStart=/bin/x\n", &none).is_err());

        let m = from_systemd(
            Path::new("worker@.service"),
            "[Service]\nExecStart=@/usr/bin/app app-%i --flag\nRestart=no\nEnvironmentFile=/nope\n",
            &none,
        )
        .unwrap();
        assert_eq!(m.file.project.name.as_deref(), Some("worker"));
        assert_eq!(
            m.file.build.start_command.as_deref(),
            Some("/usr/bin/app --flag")
        );
        assert_eq!(m.file.deploy.restart_policy.as_deref(), Some("never"));
        assert!(m
            .warnings
            .iter()
            .any(|w| w.contains("/nope could not be read")));

        let m = from_systemd(
            Path::new("a.service"),
            "[Service]\nExecStart=+!/bin/run %i\nRestart=always\n",
            &none,
        )
        .unwrap();
        assert_eq!(m.file.build.start_command.as_deref(), Some("/bin/run %i"));
        assert_eq!(m.file.deploy.restart_policy.as_deref(), Some("always"));
        assert!(m.warnings.iter().any(|w| w.contains("% specifiers")));
    }

    #[test]
    fn parses_env_files() {
        let env = parse_env_file("A=1\n\n; c\nB='x y'\nC = spaced \nD=\"q\\\"q\"\n");
        assert_eq!(env.get("A").map(String::as_str), Some("1"));
        assert_eq!(env.get("B").map(String::as_str), Some("x y"));
        assert_eq!(env.get("C").map(String::as_str), Some("spaced"));
        assert_eq!(env.get("D").map(String::as_str), Some("q\"q"));
    }
}
