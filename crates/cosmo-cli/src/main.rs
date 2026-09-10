//! The `cosmo` binary — a thin client over the control socket.
//!
//! Exit codes (cosmo-ipc contract):
//! - `0` success
//! - `1` general error
//! - `2` the gate denied the action
//! - `3` daemon not reachable
//! - `4` connection misbehaved

use std::time::Duration;

use clap::{Parser, Subcommand};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

use cosmo_ipc::{Command, DaemonMessage, Event, Request, Response, TurnResult, socket_path};

#[derive(Parser)]
#[command(
    name = "cosmo",
    version,
    about = "cosmo CLI — talks to cosmod over its control socket"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Daemon status: state, pending holds, version.
    Status,
    /// Readiness report.
    Doctor,
    /// Submit a user turn (prints events as they happen).
    Say { text: String },
    /// Confirm a held action by token (executes locally, no model).
    Confirm { token: String },
    /// Reject a held action.
    Cancel { token: String },
    /// Pause/resume new turns.
    Toggle,
}

fn main() {
    let cli = Cli::parse();
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    let code = runtime.block_on(run(cli.cmd));
    std::process::exit(code);
}

async fn run(cmd: Cmd) -> i32 {
    let path = socket_path();
    let stream = match UnixStream::connect(&path).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "daemon not running — start it with 'systemctl --user start cosmo' or run 'cosmod' in a terminal"
            );
            eprintln!("(connect {}: {e})", path.display());
            return 3;
        }
    };
    match exchange(stream, cmd).await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("protocol error: {e}");
            4
        }
    }
}

/// Send the request, collect the response, print interleaved events.
async fn exchange(
    mut stream: UnixStream,
    cmd: Cmd,
) -> Result<i32, Box<dyn std::error::Error + Send + Sync>> {
    let id = 1u64;
    let request = Request {
        id,
        cmd: match cmd {
            Cmd::Status => Command::Status,
            Cmd::Doctor => Command::Doctor,
            Cmd::Say { text } => Command::Say { text },
            Cmd::Confirm { token } => Command::Confirm { token },
            Cmd::Cancel { token } => Command::Cancel { token },
            Cmd::Toggle => Command::Toggle,
        },
    };
    let mut line = serde_json::to_string(&request)?;
    line.push('\n');
    stream.write_all(line.as_bytes()).await?;

    let mut buf = Vec::new();
    loop {
        buf.clear();
        let n = tokio::time::timeout(Duration::from_secs(120), read_line(&mut stream, &mut buf))
            .await
            .map_err(|_| "timed out waiting for the daemon")??;
        if n == 0 {
            return Err("daemon closed the connection".into());
        }
        match serde_json::from_slice::<DaemonMessage>(&buf)? {
            DaemonMessage::Event { event } => print_event(&event),
            DaemonMessage::Response { response, .. } => {
                if response_is_error(&response) {
                    return Ok(2);
                }
                return Ok(render(response));
            }
        }
    }
}

fn print_event(event: &Event) {
    match event {
        Event::State { state } => println!("[state] {state:?}"),
        Event::ToolStarted { tool, .. } => println!("[tool] {tool} …"),
        Event::ToolFinished {
            tool,
            ok,
            latency_ms,
            ..
        } => println!("[tool] {tool} -> ok={ok} ({latency_ms} ms)"),
        Event::Held { token, action } => {
            println!("[hold] {action} — confirm with: cosmo confirm {token}")
        }
        Event::HoldResolved {
            executed, summary, ..
        } => {
            println!(
                "[hold] {}",
                if *executed {
                    format!("executed: {summary}")
                } else {
                    "rejected".into()
                }
            )
        }
        Event::Reply { text } => println!("{text}"),
        Event::Usage { .. } | Event::Log { .. } => {}
    }
}

/// Exit code 2 is only for gate denials; everything else the daemon reports
/// as a structured error still renders normally.
fn response_is_error(response: &Response) -> bool {
    matches!(response, Response::Error { .. })
}

fn render(response: Response) -> i32 {
    match response {
        Response::Status(status) => {
            println!("state:     {:?}", status.state);
            println!("version:   {}", status.version);
            println!("paused:    {}", status.paused);
            if status.pending_holds.is_empty() {
                println!("pending:   none");
            } else {
                for hold in &status.pending_holds {
                    println!(
                        "pending:   {} — {} (parked at {})",
                        hold.token, hold.action, hold.parked_at_ms
                    );
                }
            }
            0
        }
        Response::Doctor(report) => {
            println!("readiness:");
            for check in &report.checks {
                let mark = if check.ok { "✓" } else { "✗" };
                println!("  {mark} {:<16} {}", check.name, check.detail);
            }
            if report.checks.iter().all(|c| c.ok) {
                0
            } else {
                1
            }
        }
        Response::Said { result } => match result {
            TurnResult::Completed { reply, held } => {
                println!("{reply}");
                for hold in held {
                    println!("held: {} — {}", hold.token, hold.action);
                }
                0
            }
            TurnResult::ConfirmedLocally { token, summary } => {
                println!("confirmed {token}: {summary}");
                0
            }
            TurnResult::ConfirmIgnored => {
                println!("nothing pending to confirm");
                0
            }
            TurnResult::Failed { reason } => {
                eprintln!("failed: {reason}");
                1
            }
        },
        Response::Confirm(outcome) => match outcome {
            cosmo_ipc::ConfirmOutcome::Executed { token, summary } => {
                println!("confirmed {token}: {summary}");
                0
            }
            cosmo_ipc::ConfirmOutcome::Unknown => {
                eprintln!("no pending hold with that token");
                1
            }
            cosmo_ipc::ConfirmOutcome::AlreadyGone { reason } => {
                eprintln!("already gone: {reason}");
                1
            }
        },
        Response::Cancelled { ok, reason } => {
            if ok {
                println!("rejected");
                0
            } else {
                eprintln!("{}", reason.unwrap_or_else(|| "unknown".into()));
                1
            }
        }
        Response::Toggled { paused } => {
            println!("{}", if paused { "paused" } else { "running" });
            0
        }
        Response::Error { message } => {
            eprintln!("error: {message}");
            1
        }
    }
}

/// Read one `\n`-terminated line into `buf` (without the newline).
async fn read_line(stream: &mut UnixStream, buf: &mut Vec<u8>) -> std::io::Result<usize> {
    let mut byte = [0u8; 1];
    loop {
        let n = stream.read(&mut byte).await?;
        if n == 0 {
            return Ok(if buf.is_empty() { 0 } else { buf.len() });
        }
        if byte[0] == b'\n' {
            return Ok(buf.len());
        }
        buf.push(byte[0]);
    }
}
