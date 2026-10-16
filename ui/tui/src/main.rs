//! `aegis-tui`: a terminal dashboard for the local Aegis daemon.
//!
//! - [`app`]: state and a pure reducer (`App::update(Msg) -> Vec<Command>`).
//! - [`ui`]: draws an `App`; no state changes.
//! - [`worker`]: the gRPC client; turns `Command`s into calls and daemon
//!   data into `Msg`s.
//!
//! This file wires them together: a terminal input thread and the worker
//! feed one channel, and the loop below applies each message and redraws.

mod app;
mod theme;
mod ui;
mod worker;

use aegis_config::Config;
use app::{App, Command, Msg};
use chrono::Utc;
use clap::Parser;
use ratatui::crossterm::event::{self, Event};
use ratatui::DefaultTerminal;
use std::path::Path;
use std::time::Duration;
use tokio::sync::mpsc::{self, UnboundedSender};

#[derive(Parser)]
#[command(
    name = "aegis-tui",
    version,
    about = "Terminal dashboard for the local Aegis daemon"
)]
struct Args {
    /// Daemon gRPC address, e.g. 127.0.0.1:50051 [default: [daemon] host and port from the config file]
    #[arg(long)]
    addr: Option<String>,
    /// Configuration file to read the daemon address from
    #[arg(long, default_value = "aegis.toml")]
    config: String,
}

/// Adds `http://` when the address has no scheme.
fn normalize_addr(raw: &str) -> String {
    if raw.contains("://") {
        raw.to_string()
    } else {
        format!("http://{}", raw)
    }
}

fn daemon_addr(args: &Args) -> String {
    let raw = args.addr.clone().unwrap_or_else(|| {
        let config = Config::load_or_default(Path::new(&args.config));
        format!("{}:{}", config.daemon.host, config.daemon.port)
    });
    normalize_addr(&raw)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let addr = daemon_addr(&args);

    // ratatui's init enters raw mode and the alternate screen, and installs a
    // panic hook that restores the terminal before the panic is printed. On
    // top of that, exit on any panic (including one in a background task) so
    // the dashboard never keeps running in a restored terminal.
    let mut terminal = ratatui::try_init()?;
    let restore_and_report = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_and_report(info);
        std::process::exit(101);
    }));

    let result = run(&mut terminal, addr).await;
    ratatui::restore();
    result
}

/// Most messages applied between two redraws, so a flood of log lines can't
/// starve the screen.
const MAX_BATCH: usize = 2000;

async fn run(terminal: &mut DefaultTerminal, addr: String) -> anyhow::Result<()> {
    let (msg_tx, mut msg_rx) = mpsc::unbounded_channel();
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    tokio::spawn(worker::run(addr.clone(), msg_tx.clone(), cmd_rx));
    spawn_input(msg_tx.clone());
    spawn_clock(msg_tx);

    let mut app = App::new(addr, Utc::now());
    terminal.draw(|f| ui::render(f, &app))?;
    while let Some(msg) = msg_rx.recv().await {
        dispatch(&mut app, msg, &cmd_tx);
        for _ in 0..MAX_BATCH {
            if app.should_quit {
                break;
            }
            match msg_rx.try_recv() {
                Ok(msg) => dispatch(&mut app, msg, &cmd_tx),
                Err(_) => break,
            }
        }
        if app.should_quit {
            break;
        }
        terminal.draw(|f| ui::render(f, &app))?;
    }
    Ok(())
}

fn dispatch(app: &mut App, msg: Msg, commands: &UnboundedSender<Command>) {
    for command in app.update(msg) {
        let _ = commands.send(command);
    }
}

/// Reads terminal input on a plain thread (crossterm's reads block).
fn spawn_input(tx: UnboundedSender<Msg>) {
    std::thread::spawn(move || loop {
        match event::poll(Duration::from_millis(250)) {
            Ok(true) => {
                let msg = match event::read() {
                    Ok(Event::Key(key)) => Msg::Key(key),
                    Ok(Event::Resize(..)) => Msg::Resize,
                    Ok(_) => continue,
                    Err(_) => return,
                };
                if tx.send(msg).is_err() {
                    return;
                }
            }
            Ok(false) if tx.is_closed() => return,
            Ok(false) => {}
            Err(_) => return,
        }
    });
}

/// Ticks once a second so uptimes stay current.
fn spawn_clock(tx: UnboundedSender<Msg>) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        loop {
            tick.tick().await;
            if tx.send(Msg::Tick(Utc::now())).is_err() {
                return;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_get_a_scheme() {
        assert_eq!(normalize_addr("127.0.0.1:50051"), "http://127.0.0.1:50051");
        assert_eq!(normalize_addr("http://[::1]:7000"), "http://[::1]:7000");
    }

    #[test]
    fn addr_flag_overrides_config() {
        let args = Args::parse_from(["aegis-tui", "--addr", "localhost:9999"]);
        assert_eq!(daemon_addr(&args), "http://localhost:9999");
        let args = Args::parse_from(["aegis-tui", "--config", "does-not-exist.toml"]);
        assert!(daemon_addr(&args).starts_with("http://"));
    }
}
