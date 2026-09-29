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
    /// Start listening, as if the trigger key were held; run it again to
    /// stop. The one that started prints the transcript. Bind it to a
    /// COSMIC shortcut (`Spawn`) when holding a key isn't an option.
    Listen,
    /// Print every transcript as it happens (partials update in place on a
    /// terminal; only finals when piped). Ctrl+C stops.
    Transcripts,
    /// Open the OpenAI key page, read the key from stdin (echo disabled),
    /// store it in the Secret Service.
    AuthLogin,
    /// Delete the stored key from the Secret Service.
    AuthLogout,
    /// Report whether a key is resolvable and from which source.
    AuthStatus,
    /// Voices: list them, hear one, pick one (the daemon plays audio; this
    /// CLI never does).
    Voice {
        #[command(subcommand)]
        cmd: VoiceCmd,
    },
}

#[derive(Subcommand)]
enum VoiceCmd {
    /// Voices grouped by accent; the active one is marked.
    List {
        /// Another provider's voices (default: the active provider).
        #[arg(long)]
        provider: Option<String>,
    },
    /// Hear a voice say a fixed sample line.
    Preview {
        voice: String,
        #[arg(long)]
        provider: Option<String>,
    },
    /// Make a voice the active one: renders its phrases, then switches and
    /// saves it to config.ron.
    Set {
        voice: String,
        #[arg(long)]
        provider: Option<String>,
    },
}

fn main() {
    let cli = Cli::parse();
    let runtime = tokio::runtime::Runtime::new().expect("tokio runtime");
    let code = runtime.block_on(run(cli.cmd));
    std::process::exit(code);
}

async fn run(cmd: Cmd) -> i32 {
    // Auth commands are local (Secret Service), not daemon round trips.
    match cmd {
        Cmd::AuthLogin => return auth_login().await,
        Cmd::AuthLogout => return auth_logout().await,
        Cmd::AuthStatus => {
            println!("{}", cosmo_reason::secret::auth_status().await);
            return 0;
        }
        _ => {}
    }
    let path = socket_path();
    let mut stream = match UnixStream::connect(&path).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "daemon not running — start it with 'systemctl --user start cosmo' or run 'cosmod' in a terminal"
            );
            eprintln!("(connect {}: {e})", path.display());
            return 3;
        }
    };
    let result = match cmd {
        Cmd::Listen => listen(stream).await,
        Cmd::Transcripts => watch_transcripts(&mut stream).await,
        cmd => exchange(stream, cmd).await,
    };
    match result {
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
            Cmd::Listen | Cmd::Transcripts => unreachable!("handled by their own loops"),
            Cmd::Voice { cmd } => match cmd {
                VoiceCmd::List { provider } => Command::VoiceList { provider },
                VoiceCmd::Preview { voice, provider } => Command::VoicePreview { provider, voice },
                VoiceCmd::Set { voice, provider } => Command::VoiceSet { provider, voice },
            },
            Cmd::AuthLogin | Cmd::AuthLogout | Cmd::AuthStatus => {
                unreachable!("auth subcommands are handled before the daemon connection")
            }
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
            // A structured error renders its message and exits 1 (general
            // error). Exit 2 is reserved for gate denials, which arrive as
            // turn results, not as `Response::Error`.
            DaemonMessage::Response { response, .. } => return Ok(render(response)),
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
        Event::VoiceCacheProgress {
            voice, done, total, ..
        } => println!("[voice] rendering {voice} phrases {done}/{total}"),
        Event::VoiceCacheDone {
            voice, ok, detail, ..
        } => println!(
            "[voice] {voice} {}: {detail}",
            if *ok { "ready" } else { "FAILED" }
        ),
        Event::Transcript {
            text,
            r#final: true,
            ..
        } => println!("[heard] {text}"),
        Event::Usage { .. } | Event::Log { .. } | Event::Transcript { .. } => {}
    }
}

/// Transcripts on a terminal: the partial rewrites one line, the final
/// replaces it. Piped, only finals are printed, one per line.
struct TranscriptPrinter {
    tty: bool,
}

impl TranscriptPrinter {
    fn new() -> Self {
        use std::io::IsTerminal;
        Self {
            tty: std::io::stdout().is_terminal(),
        }
    }

    /// Print a transcript event; true if it was a final.
    fn print(&self, event: &Event) -> bool {
        use std::io::Write;
        match event {
            Event::Transcript {
                text,
                r#final: false,
                ..
            } => {
                if self.tty {
                    print!("\r\x1b[2K\x1b[2m{text}\x1b[0m");
                    let _ = std::io::stdout().flush();
                }
                false
            }
            Event::Transcript {
                text, latency_ms, ..
            } => {
                if self.tty {
                    print!("\r\x1b[2K");
                    let latency = latency_ms.map(|ms| format!("  \x1b[2m({ms} ms)\x1b[0m"));
                    println!("{text}{}", latency.unwrap_or_default());
                } else {
                    println!("{text}");
                }
                true
            }
            Event::Log { line } if line.starts_with("not listening") => {
                eprintln!("{line}");
                false
            }
            _ => false,
        }
    }
}

/// `cosmo listen`: toggle, then (if this call started the recording) show
/// it until its transcript arrives.
async fn listen(mut stream: UnixStream) -> Result<i32, Box<dyn std::error::Error + Send + Sync>> {
    let mut line = serde_json::to_string(&Request {
        id: 1,
        cmd: Command::Listen,
    })?;
    line.push('\n');
    stream.write_all(line.as_bytes()).await?;
    let printer = TranscriptPrinter::new();
    let mut started = false;
    let mut buf = Vec::new();
    loop {
        buf.clear();
        // A recording is capped at 60 s; this is only a stuck-daemon guard.
        let n = tokio::time::timeout(Duration::from_secs(120), read_line(&mut stream, &mut buf))
            .await
            .map_err(|_| "timed out waiting for the transcript")??;
        if n == 0 {
            return Err("daemon closed the connection".into());
        }
        match serde_json::from_slice::<DaemonMessage>(&buf)? {
            DaemonMessage::Response {
                response: Response::Listening { active: true },
                ..
            } => started = true,
            DaemonMessage::Response {
                response: Response::Listening { active: false },
                ..
            } => {
                eprintln!("stopped (the `cosmo listen` that started it prints the transcript)");
                return Ok(0);
            }
            DaemonMessage::Response { response, .. } => return Ok(render(response)),
            // Before our response arrives, a transcript is someone else's.
            DaemonMessage::Event { event } if started => {
                if printer.print(&event) {
                    return Ok(0);
                }
            }
            DaemonMessage::Event { .. } => {}
        }
    }
}

/// `cosmo transcripts`: print transcripts until the daemon goes away.
async fn watch_transcripts(
    stream: &mut UnixStream,
) -> Result<i32, Box<dyn std::error::Error + Send + Sync>> {
    let printer = TranscriptPrinter::new();
    let mut buf = Vec::new();
    loop {
        buf.clear();
        if read_line(stream, &mut buf).await? == 0 {
            eprintln!("daemon stopped");
            return Ok(3);
        }
        if let DaemonMessage::Event { event } = serde_json::from_slice::<DaemonMessage>(&buf)? {
            printer.print(&event);
        }
    }
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
            TurnResult::Reflexed { action, summary } => {
                println!("{action} — {summary}");
                0
            }
            TurnResult::ConfirmNeedsKey => {
                eprintln!("not confirmed: hold the trigger key while you say it");
                1
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
        Response::Confirm { outcome } => match outcome {
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
        Response::Listening { active } => {
            println!("{}", if active { "listening" } else { "stopped" });
            0
        }
        Response::Toggled { paused } => {
            println!("{}", if paused { "paused" } else { "running" });
            0
        }
        Response::Voices {
            provider,
            active,
            mut voices,
        } => {
            voices.sort_by(|a, b| (&a.accent, &a.id).cmp(&(&b.accent, &b.id)));
            println!("{provider} voices:");
            let mut accent = "";
            for v in &voices {
                if v.accent != accent {
                    accent = &v.accent;
                    println!("  {accent}");
                }
                let mark = if active.as_deref() == Some(v.id.as_str()) {
                    "*"
                } else {
                    " "
                };
                let gender = v.gender.as_deref().unwrap_or("");
                println!("   {mark} {:<12} {:<10} {gender}", v.id, v.label);
            }
            if active.is_some() {
                println!("(* active — change with: cosmo voice set <id>)");
            }
            0
        }
        Response::VoicePreviewed { provider, voice } => {
            println!("previewed {provider}/{voice}");
            0
        }
        Response::VoiceSet {
            provider,
            voice,
            persisted_to,
        } => {
            println!("voice set to {provider}/{voice} (saved to {persisted_to})");
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

/// `cosmo auth login`: open the key-creation page, read the key with echo
/// disabled, store it in the Secret Service (plan §1.4 — the whole browser
/// story; there is deliberately no OAuth flow).
async fn auth_login() -> i32 {
    println!("Opening https://platform.openai.com/api-keys — create or copy an API key.");
    let _ = std::process::Command::new("xdg-open")
        .arg("https://platform.openai.com/api-keys")
        .spawn();

    let key = read_line_echo_disabled().expect("read key from stdin");
    let key = key.trim().to_string();
    if key.is_empty() {
        eprintln!("no key entered");
        return 1;
    }
    match cosmo_reason::secret::store_key(&key).await {
        Ok(()) => {
            println!("key stored in the Secret Service (application=cosmo, provider=openai)");
            0
        }
        Err(e) => {
            eprintln!("storing the key failed: {e}");
            1
        }
    }
}

/// `cosmo auth logout`: delete the stored key.
async fn auth_logout() -> i32 {
    match cosmo_reason::secret::delete_key().await {
        Ok(()) => {
            println!("key deleted from the Secret Service");
            0
        }
        Err(e) => {
            eprintln!("deleting the key failed: {e}");
            1
        }
    }
}

/// Read one line from stdin with terminal echo disabled (tty) or plain
/// (piped — CI). The raw key never hits the terminal.
///
/// The `unsafe` blocks here are libc termios calls: single-threaded CLI,
/// save/restore around exactly one read, fd 0 only.
#[allow(unsafe_code)]
fn read_line_echo_disabled() -> std::io::Result<String> {
    use std::io::{BufRead, Write};
    let stdin = std::io::stdin();
    let mut line = String::new();
    let mut stdout = std::io::stdout();
    write!(stdout, "API key: ")?;
    stdout.flush()?;
    if let Some(term) = std::env::var("TERM").ok().filter(|_| {
        // Only disable echo when stdin is a TTY; piped input (CI) reads plain.
        unsafe { libc::isatty(0) == 1 }
    }) {
        let _ = term;
        // SAFETY: single-threaded CLI; termios save/restore around one read.
        let mut saved = libc::termios {
            c_iflag: 0,
            c_oflag: 0,
            c_cflag: 0,
            c_lflag: 0,
            c_line: 0,
            c_cc: [0; 32],
            c_ispeed: 0,
            c_ospeed: 0,
        };
        unsafe {
            libc::tcgetattr(0, &mut saved);
            let mut noecho = saved;
            noecho.c_lflag &= !libc::ECHO;
            libc::tcsetattr(0, libc::TCSANOW, &noecho);
        }
        let res = stdin.lock().read_line(&mut line);
        unsafe {
            libc::tcsetattr(0, libc::TCSANOW, &saved);
        }
        println!();
        res?;
    } else {
        stdin.lock().read_line(&mut line)?;
    }
    Ok(line)
}
