# Claude assistant

An optional chat in the dashboard: Claude Code runs on the Aegis server and
answers questions such as "why did the last deployment of api fail?" or "which
app is logging errors?". It is **off by default**, and only the **owner** (the
first admin account) can use it. API tokens cannot.

## Set it up

1. **Account → Claude assistant → Turn assistant on**.
2. **Install Claude Code**. This runs Anthropic's installer
   (`https://claude.ai/install.sh`) as the Aegis user, into
   `<data_dir>/assistant/home`. It needs `curl` and internet access. A
   `claude` already on the daemon's `PATH` is used instead.
3. Paste an Anthropic API key, or a token from `claude setup-token`. It is
   stored encrypted and never shown again. Usage is billed to that account.
4. Open **Assistant** in the top bar and ask.

Linux and macOS servers only.

## What Claude can see and do

For each question Aegis writes a snapshot to `<data_dir>/assistant/workspace`:
app status and usage, the last 100 deployments, the last 300 events, and up to
500 lines of each app's output and latest build log.

- That snapshot is **sent to Anthropic**. If your apps log personal data or
  secrets, it goes too.
- Variable and secret values are never copied into it, and apps hidden behind a
  password are left out unless your session has unlocked them.
- Claude runs in restricted mode with the file reading tools only (`Read`,
  `Grep`, `Glob`), confined to the snapshot. It has no shell and no network
  tool, so it **cannot change anything**. It suggests what you can do.
- Logs come from outside, so they could contain text written to steer the
  assistant. The worst it can do is give a wrong answer.

One question runs at a time, for at most five minutes.

## Turning it off

**Turn assistant off** hides the page and stops it from running. **Remove**
deletes the stored key. To remove Claude Code itself, delete
`<data_dir>/assistant`.
