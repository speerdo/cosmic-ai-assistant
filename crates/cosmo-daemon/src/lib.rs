//! The cosmo daemon: a single long-lived process running the state machine
//! (`Idle / Listening / Thinking / Acting / Waiting / Speaking`) that owns
//! everything — microphone, hotkey, resident models, tools, and the control
//! socket.
//!
//! ## Invariant
//!
//! The COSMIC panel spawns **one applet process per output**. The daemon is a
//! separate process precisely so that multiplicity doesn't matter: the applet
//! and overlay are thin clients over the control socket and never own engine
//! state. Do not "simplify" by moving the engine into the applet.
//!
//! ## Socket lifecycle (plan §1.1)
//!
//! - single instance: binding fails loudly if another daemon lives
//! - stale-socket cleanup: an unclean exit leaves `cosmo.sock` behind; a
//!   probe-connect distinguishes a live daemon (refused → exit) from a dead
//!   socket (unlinked → bind)
//! - SIGTERM/SIGINT unlink the socket on the way out
//! - client-side "not running" vs "refused" are distinct (cosmo-ipc exit
//!   codes 3 and 4)

#[cfg(feature = "ears")]
pub mod ears;
pub mod engine;
pub mod speech;
pub mod toolhost;

use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::broadcast;

use cosmo_ipc::{DaemonMessage, Event, Request, State, socket_path};

/// Run the daemon until SIGTERM/SIGINT. Returns after unlinking the socket.
pub async fn run() -> anyhow::Result<()> {
    let cfg = cosmo_config::load()?;
    let path = socket_path();

    let listener = bind_socket(&path).await?;
    tracing::info!(path = %path.display(), "listening");

    let (events_tx, _) = broadcast::channel::<Event>(256);
    let mut engine = engine::Engine::new(cfg.clone(), events_tx.clone()).await;
    let gate = attach_speech(&mut engine);
    let engine = Arc::new(engine);
    attach_ears(&engine, gate, &cfg);
    engine.warm_speech();
    engine.refresh_lock();
    // Agent connection: failure is not fatal (graceful absence; doctor
    // reports it and `say` errors per turn).
    if let Err(e) = engine.connect_tools().await {
        tracing::warn!(error = %e, "agent connect failed; native tools still available");
    }
    engine.set_state(State::Idle);

    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;

    loop {
        tokio::select! {
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, _)) => {
                        let engine = Arc::clone(&engine);
                        let events = events_tx.subscribe();
                        tokio::spawn(serve(stream, engine, events));
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "accept failed");
                    }
                }
            }
            _ = sigterm.recv() => break,
            _ = sigint.recv() => break,
        }
    }

    drop(listener);
    match std::fs::remove_file(&path) {
        Ok(()) => tracing::info!("socket unlinked; bye"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => tracing::warn!(error = %e, "socket unlink failed"),
    }
    Ok(())
}

/// Start the PipeWire player and hand it to the engine. Failure is not
/// fatal: the daemon answers in text and `doctor` says why it is silent.
/// Returns playback's half-duplex gate, for capture to honour.
#[cfg(feature = "speech")]
fn attach_speech(engine: &mut engine::Engine) -> Option<cosmo_audio::SpeechGate> {
    match cosmo_audio::Player::start() {
        Ok(player) => {
            tracing::info!("speech: PipeWire player started");
            let gate = player.gate();
            engine.attach_speech(Arc::new(player));
            Some(gate)
        }
        Err(e) => {
            tracing::warn!(error = %e, "speech unavailable; replies are text only");
            engine.speech_unavailable(e.to_string());
            None
        }
    }
}

#[cfg(not(feature = "speech"))]
fn attach_speech(_engine: &mut engine::Engine) -> Option<()> {
    None
}

/// Start the microphone, hotkey and speech models (phase 3). Failure is
/// not fatal: `doctor` and `cosmo listen` say why there are no ears.
#[cfg(feature = "ears")]
fn attach_ears(
    engine: &Arc<engine::Engine>,
    gate: Option<cosmo_audio::SpeechGate>,
    cfg: &cosmo_config::Config,
) {
    // No player means nothing cosmo says can reach the mic: a gate that
    // never closes is correct then.
    let gate = gate.unwrap_or_default();
    let host: Arc<dyn ears::Host> = Arc::clone(engine) as _;
    match ears::start(host, gate, cfg) {
        Ok(ears) => {
            tracing::info!("ears: capture and hotkey up, models loading");
            engine.attach_ears(ears);
        }
        Err(e) => {
            tracing::warn!(error = %e, "ears unavailable");
            engine.ears_unavailable(e);
        }
    }
}

#[cfg(not(feature = "ears"))]
fn attach_ears<G>(_engine: &Arc<engine::Engine>, _gate: Option<G>, _cfg: &cosmo_config::Config) {}

async fn write_msg(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    msg: &DaemonMessage,
) -> std::io::Result<()> {
    let out = serde_json::to_string(msg).unwrap_or_default() + "\n";
    writer.write_all(out.as_bytes()).await
}

/// Bind the control socket, cleaning up a stale one first.
///
/// A leftover socket is *stale* only when nothing is listening: we probe by
/// connecting. ECONNREFUSED ⇒ dead socket, unlink and bind. Connect OK ⇒ a
/// daemon already owns it; refuse to start (single-instance guard).
async fn bind_socket(path: &std::path::Path) -> anyhow::Result<UnixListener> {
    if path.exists() {
        match UnixStream::connect(path).await {
            // Something answered: a live daemon owns the socket.
            Ok(_) => anyhow::bail!(
                "another cosmod already holds {} — stop it first",
                path.display()
            ),
            // Nobody home: unclean exit left the socket behind.
            Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
                tracing::warn!(path = %path.display(), "removing stale socket");
                std::fs::remove_file(path)?;
            }
            // Odd error but still evidence of a live owner; fail closed.
            Err(e) => anyhow::bail!("socket {} exists and connect failed: {e}", path.display()),
        }
    }
    let listener =
        UnixListener::bind(path).map_err(|e| anyhow::anyhow!("bind {}: {e}", path.display()))?;
    // The runtime dir is shared; the socket should be user-only.
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

/// One client connection: newline-delimited JSON request/response plus a
/// background task forwarding broadcast events.
async fn serve(
    stream: UnixStream,
    engine: Arc<engine::Engine>,
    mut events: broadcast::Receiver<Event>,
) {
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();

    loop {
        tokio::select! {
            line = lines.next_line() => {
                match line {
                    Ok(Some(line)) if line.trim().is_empty() => continue,
                    Ok(Some(line)) => {
                        match serde_json::from_str::<Request>(&line) {
                            Ok(req) => {
                                // Events keep flowing while the command runs:
                                // `say` shows its tool calls live and `voice
                                // set` its render progress. Awaiting the
                                // command alone would hold every event until
                                // after the response — which the CLI stops
                                // reading at.
                                let handling = engine.handle(req.cmd);
                                tokio::pin!(handling);
                                let response = loop {
                                    tokio::select! {
                                        response = &mut handling => break Some(response),
                                        event = events.recv() => match event {
                                            Ok(event) => {
                                                if write_msg(&mut writer, &DaemonMessage::Event { event }).await.is_err() {
                                                    break None;
                                                }
                                            }
                                            Err(broadcast::error::RecvError::Lagged(n)) => {
                                                tracing::debug!(skipped = n, "event lag");
                                            }
                                            // No more events will come; just finish the command.
                                            Err(broadcast::error::RecvError::Closed) => {
                                                break Some((&mut handling).await);
                                            }
                                        },
                                    }
                                };
                                let Some(response) = response else { break };
                                // Events the command emitted in its last
                                // instant are already queued: send them
                                // before the response, which is where the
                                // client stops reading. `voice set` lost its
                                // final progress and "ready" lines to this.
                                let mut dropped = false;
                                while let Ok(event) = events.try_recv() {
                                    if write_msg(&mut writer, &DaemonMessage::Event { event }).await.is_err() {
                                        dropped = true;
                                        break;
                                    }
                                }
                                if dropped {
                                    break;
                                }
                                let msg = DaemonMessage::Response { id: req.id, response };
                                if write_msg(&mut writer, &msg).await.is_err() {
                                    break;
                                }
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "malformed request");
                                break; // protocol misbehaviour: close
                            }
                        }
                    }
                    Ok(None) | Err(_) => break,
                }
            }
            event = events.recv() => {
                match event {
                    Ok(event) => {
                        let msg = DaemonMessage::Event { event };
                        let out = serde_json::to_string(&msg).unwrap_or_default() + "\n";
                        if writer.write_all(out.as_bytes()).await.is_err() {
                            break;
                        }
                    }
                    // No live sender; nothing to forward.
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::debug!(skipped = n, "event lag");
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }
    }
}
