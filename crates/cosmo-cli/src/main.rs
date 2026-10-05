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

mod setup;

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
    /// First-run setup: about you (name, home town for the weather,
    /// units), the local models, and a reasoning provider. Safe to re-run.
    Setup,
    /// What the profile holds (`cosmo setup` changes it).
    Profile,
    /// Readiness report: this machine's install, then the daemon's view.
    Doctor,
    /// The local models: which are present. `cosmo models fetch` downloads
    /// the missing ones (checksummed) and restarts the daemon.
    Models {
        #[command(subcommand)]
        cmd: Option<ModelsCmd>,
    },
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
    /// Connect a reasoning provider. OpenRouter signs in with your
    /// browser (no key to copy); the others open their key page and you
    /// paste the key (echo off). Stored in the Secret Service.
    AuthLogin {
        /// Which provider (default: `provider` in config.ron).
        #[arg(long)]
        provider: Option<String>,
        /// Paste a key even where browser sign-in exists (e.g. over SSH).
        #[arg(long)]
        paste: bool,
    },
    /// Reasoning providers: list them with their connection state, or
    /// switch to one (live, and saved to config.ron).
    Use {
        /// The provider to switch to; omit to list them.
        provider: Option<String>,
        /// The model (default: the provider's own default).
        #[arg(long, default_value = "")]
        model: String,
    },
    /// Delete a provider's stored key from the Secret Service.
    AuthLogout {
        #[arg(long)]
        provider: Option<String>,
    },
    /// Report whether the provider's key is resolvable and from which source.
    AuthStatus {
        #[arg(long)]
        provider: Option<String>,
    },
    /// Voices: list them, hear one, pick one (the daemon plays audio; this
    /// CLI never does).
    Voice {
        #[command(subcommand)]
        cmd: VoiceCmd,
    },
}

#[derive(Subcommand)]
enum ModelsCmd {
    /// Download the default models (~1.6 GB) into ~/.cache/cosmo/models.
    /// Re-running is cheap: verified files are skipped.
    Fetch {
        /// Passed to the fetcher, e.g. `--variant q8` for the smaller voice
        /// model.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
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
        Cmd::AuthLogin { provider, paste } => return auth_login(provider, paste).await,
        Cmd::AuthLogout { provider } => {
            let Some(provider) = auth_provider(provider) else {
                return 1;
            };
            return auth_logout(provider.name).await;
        }
        Cmd::AuthStatus { provider } => {
            let Some(provider) = auth_provider(provider) else {
                return 1;
            };
            println!(
                "{}: {}",
                provider.name,
                cosmo_reason::secret::auth_status(provider.name).await
            );
            return 0;
        }
        Cmd::Models { cmd: None } => return setup::models_status(),
        Cmd::Profile => return setup::show_profile(),
        Cmd::Setup => return first_run().await,
        Cmd::Models {
            cmd: Some(ModelsCmd::Fetch { args }),
        } => return setup::models_fetch(&args),
        _ => {}
    }
    // Doctor's local half first: it has to work with the daemon down.
    let doctor = matches!(cmd, Cmd::Doctor);
    let local_ok = if doctor {
        let checks = setup::local_checks();
        println!("this machine:");
        print_checks(&checks);
        println!();
        checks.iter().all(|c| c.ok)
    } else {
        true
    };
    let path = socket_path();
    let mut stream = match UnixStream::connect(&path).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "daemon not running — start it with 'systemctl --user start cosmo' or run 'cosmod' in a terminal"
            );
            eprintln!("(connect {}: {e})", path.display());
            if doctor {
                eprintln!("(`journalctl --user -u cosmo` says why it isn't running)");
            }
            return 3;
        }
    };
    let result = match cmd {
        Cmd::Listen => listen(stream).await,
        Cmd::Transcripts => watch_transcripts(&mut stream).await,
        cmd => exchange(stream, cmd).await,
    };
    match result {
        Ok(0) if !local_ok => 1,
        Ok(code) => code,
        Err(e) => {
            eprintln!("protocol error: {e}");
            4
        }
    }
}

fn print_checks(checks: &[cosmo_ipc::DoctorCheck]) {
    for check in checks {
        let mark = match (check.ok, check.warn) {
            (true, false) => "✓",
            (true, true) => "!",
            (false, _) => "✗",
        };
        println!("  {mark} {:<16} {}", check.name, check.detail);
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
            Cmd::Use { provider: None, .. } => Command::Reasoning,
            Cmd::Use {
                provider: Some(provider),
                model,
            } => Command::ReasoningSet { provider, model },
            Cmd::Listen | Cmd::Transcripts => unreachable!("handled by their own loops"),
            Cmd::Voice { cmd } => match cmd {
                VoiceCmd::List { provider } => Command::VoiceList { provider },
                VoiceCmd::Preview { voice, provider } => Command::VoicePreview { provider, voice },
                VoiceCmd::Set { voice, provider } => Command::VoiceSet { provider, voice },
            },
            Cmd::AuthLogin { .. }
            | Cmd::AuthLogout { .. }
            | Cmd::AuthStatus { .. }
            | Cmd::Models { .. }
            | Cmd::Setup
            | Cmd::Profile => {
                unreachable!("local subcommands are handled before the daemon connection")
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
        Event::SignIn { detail, .. } => println!("[sign-in] {detail}"),
        Event::Usage { .. }
        | Event::Log { .. }
        | Event::Transcript { .. }
        | Event::Level { .. } => {}
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
            println!("daemon:");
            print_checks(&report.checks);
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
        Response::Reasoning(info) => {
            for p in &info.providers {
                let mark = if p.name == info.active { "▸" } else { " " };
                let how = match (p.connect, p.connected) {
                    (cosmo_ipc::Connect::Local, true) => format!(
                        "local server: {} on this computer, {} via Ollama's cloud",
                        p.models.len(),
                        p.remote_models.len()
                    ),
                    (cosmo_ipc::Connect::Local, false) => "no local server".into(),
                    (_, true) => "connected".into(),
                    (cosmo_ipc::Connect::Browser, false) => {
                        format!("sign in: cosmo auth-login --provider {}", p.name)
                    }
                    (cosmo_ipc::Connect::Key, false) => {
                        format!("add a key: cosmo auth-login --provider {}", p.name)
                    }
                };
                println!("{mark} {:<12} {:<26} {how}", p.name, p.label);
            }
            println!("in use: {} · {}", info.active, info.model);
            0
        }
        Response::ReasoningSet { provider, model } => {
            println!("cosmo now reasons with {provider} · {model}");
            0
        }
        Response::SignInUrl { url, .. } => {
            println!("{url}");
            0
        }
        Response::KeyStored { provider } => {
            println!("{provider} key stored");
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

/// The provider an auth command is about: the one named, else the
/// config's. `None` (after saying why) if it isn't one cosmo knows.
fn auth_provider(named: Option<String>) -> Option<&'static cosmo_reason::provider::Preset> {
    let name = named.unwrap_or_else(|| {
        cosmo_config::load()
            .map(|c| c.provider)
            .unwrap_or_else(|_| cosmo_config::Config::default().provider)
    });
    let found = cosmo_reason::provider::preset(&name);
    if found.is_none() {
        eprintln!(
            "unknown provider {name:?} (known: {})",
            cosmo_reason::provider::names()
        );
    }
    found
}

/// `cosmo auth-login`: open the provider's key page, read the key with echo
/// disabled, store it in the Secret Service under that provider (plan §1.4 —
/// the whole browser story; there is deliberately no OAuth flow).
/// `cosmo setup`: the questions worth asking once, in order.
async fn first_run() -> i32 {
    println!("Setting up cosmo.\n\n— 1. About you —");
    if let Err(e) = setup::setup_profile().await {
        eprintln!("couldn't save the profile: {e}");
        return 1;
    }

    println!("\n— 2. Local models (speech recognition and the voice) —");
    let root = cosmo_config::models::root().unwrap_or_default();
    let missing: Vec<_> = cosmo_config::models::default_set(&root)
        .into_iter()
        .filter(|m| !m.present)
        .map(|m| m.name)
        .collect();
    if missing.is_empty() {
        println!("All present.");
    } else if setup::yes(
        &format!(
            "Missing: {}. Download them now (about 1.6 GB)?",
            missing.join(", ")
        ),
        true,
    ) && setup::models_fetch(&[]) != 0
    {
        eprintln!("(re-run `cosmo models fetch` later; verified files are kept)");
    }

    println!("\n— 3. Reasoning (the model that answers) —");
    match cosmo_ipc::client::request(Command::Reasoning).await {
        Ok(Response::Reasoning(info))
            if info
                .providers
                .iter()
                .any(|p| p.name == info.active && p.connected) =>
        {
            println!("Connected: {} · {}.", info.active, info.model);
        }
        _ => {
            println!(
                "Not connected yet. Choose one (you can change it any time, also from the \
                 panel applet):\n  1) OpenRouter: sign in with your browser, many models\n  \
                 2) Another provider, with an API key\n  3) A model on this computer (Ollama)\n  \
                 4) Later"
            );
            match setup::ask("Which", "1").as_str() {
                "1" => {
                    auth_login(Some("openrouter".into()), false).await;
                }
                "2" => {
                    let p = setup::ask(
                        &format!("Which provider ({})", cosmo_reason::provider::names()),
                        "openai",
                    );
                    auth_login(Some(p), false).await;
                }
                "3" => {
                    let cmd = Command::ReasoningSet {
                        provider: "local".into(),
                        model: String::new(),
                    };
                    match cosmo_ipc::client::request(cmd).await {
                        Ok(Response::ReasoningSet { model, .. }) => println!(
                            "cosmo now uses the local model {model}. If Ollama isn't installed: \
                             https://ollama.com/download, then `ollama pull {model}`."
                        ),
                        _ => println!("Start the daemon first, then: cosmo use local"),
                    }
                }
                _ => println!("Later, then: cosmo auth-login, or the applet's Reasoning section."),
            }
        }
    }
    println!("\nDone. `cosmo doctor` shows what's working; `cosmo setup` again changes any of it.");
    0
}

async fn auth_login(named: Option<String>, paste: bool) -> i32 {
    let Some(provider) = auth_provider(named) else {
        return 1;
    };
    if !provider.needs_key {
        println!(
            "{} needs no sign-in: the model runs on this computer. Start a local server \
             (Ollama: {}), then `cosmo use {}`.",
            provider.label, provider.key_page, provider.name
        );
        return 0;
    }
    if let Some(note) = provider.note {
        println!("Note: {note}.");
    }
    let key = match (provider.browser_login, paste) {
        (Some(login), false) => {
            println!(
                "Signing in to {} with your browser: approve cosmo there, and \
                 the key comes back here by itself (nothing to copy).",
                provider.name
            );
            let signed_in = cosmo_reason::login::browser_login(&login, |url| {
                println!("If no browser opens, visit:\n  {url}");
                let _ = std::process::Command::new("xdg-open").arg(url).spawn();
            })
            .await;
            match signed_in {
                Ok(key) => key,
                Err(e) => {
                    eprintln!("{e}");
                    eprintln!(
                        "(or paste a key instead: cosmo auth-login --provider {} --paste)",
                        provider.name
                    );
                    return 1;
                }
            }
        }
        (login, _) => {
            if login.is_none() {
                println!(
                    "{} has no browser sign-in for apps, so this takes an API key.",
                    provider.name
                );
            }
            println!(
                "Opening {} — create or copy a {} API key, then paste it here.",
                provider.key_page, provider.name
            );
            let _ = std::process::Command::new("xdg-open")
                .arg(provider.key_page)
                .spawn();
            let key = read_line_echo_disabled().expect("read key from stdin");
            let key = key.trim().to_string();
            if key.is_empty() {
                eprintln!("no key entered");
                return 1;
            }
            key
        }
    };
    if let Err(e) = cosmo_reason::secret::store_key(provider.name, &key).await {
        eprintln!("storing the key failed: {e}");
        return 1;
    }
    println!(
        "Connected: the {} key is in the Secret Service (application=cosmo, provider={}).",
        provider.name, provider.name
    );
    let configured = cosmo_config::load().map(|c| c.provider).unwrap_or_default();
    if configured != provider.name && !use_provider(provider).await {
        println!(
            "cosmo still reasons with {configured:?}; `cosmo auth-login --provider {}` again, \
             or set `provider: \"{}\",` in {}, to switch",
            provider.name,
            provider.name,
            cosmo_config::config_path().display()
        );
    }
    0
}

/// After connecting a provider cosmo isn't set to use: offer to switch to
/// it (a terminal only). True when switched.
#[allow(unsafe_code)]
async fn use_provider(provider: &cosmo_reason::provider::Preset) -> bool {
    use std::io::Write;
    // SAFETY: isatty on fd 0 only reads the descriptor's state.
    if unsafe { libc::isatty(0) } != 1 {
        return false;
    }
    print!(
        "Use {} for reasoning now (model: its default, {})? [Y/n] ",
        provider.name, provider.default_model
    );
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    if std::io::stdin().read_line(&mut answer).is_err()
        || answer.trim().to_lowercase().starts_with('n')
    {
        return false;
    }
    // Through the daemon when it runs (live, no restart), else the file.
    let cmd = Command::ReasoningSet {
        provider: provider.name.to_owned(),
        model: String::new(),
    };
    match cosmo_ipc::client::request(cmd).await {
        Ok(Response::ReasoningSet { provider, model }) => {
            println!("cosmo now reasons with {provider} · {model}");
        }
        Ok(Response::Error { message }) => {
            eprintln!("couldn't switch: {message}");
            return false;
        }
        _ => {
            // The daemon isn't running: write the file it'll read at start.
            let path = cosmo_config::config_path();
            let fields = [
                ("provider", provider.name),
                ("model", ""),
                ("api_base", ""),
                ("api_format", ""),
            ];
            if let Err(e) = cosmo_config::set_string_fields(&path, &fields) {
                eprintln!("couldn't update {}: {e}", path.display());
                return false;
            }
            println!(
                "cosmo will reason with {} when the daemon starts",
                provider.name
            );
        }
    }
    true
}

/// `cosmo auth-logout`: delete a provider's stored key.
async fn auth_logout(provider: &str) -> i32 {
    match cosmo_reason::secret::delete_key(provider).await {
        Ok(()) => {
            println!("{provider} key deleted from the Secret Service");
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
