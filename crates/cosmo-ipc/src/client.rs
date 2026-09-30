//! A client for the overlay and the applet (phase-6 spec §6.1), behind the
//! `client` feature.
//!
//! - [`subscribe`]: the daemon's event stream, which **survives daemon
//!   restarts**. It reports [`Update::Connected`] / [`Update::Disconnected`]
//!   and keeps retrying, so a face started before the daemon, or one that
//!   outlives it, just shows nothing until it's back.
//! - [`request`]: one command, one response, on its own connection (a
//!   confirm click, a voice preview).

use std::path::PathBuf;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::mpsc;

use crate::{Command, DaemonMessage, Event, Request, Response, socket_path};

/// What a subscription reports.
#[derive(Debug, Clone)]
pub enum Update {
    Connected,
    Disconnected,
    Event(Event),
}

/// How long to wait between attempts while the daemon is away.
pub const RETRY: Duration = Duration::from_secs(1);

/// Follow the daemon's events at the default socket until the receiver is
/// dropped.
pub fn subscribe() -> mpsc::UnboundedReceiver<Update> {
    subscribe_at(socket_path())
}

/// [`subscribe`] at a given socket path (tests).
pub fn subscribe_at(path: PathBuf) -> mpsc::UnboundedReceiver<Update> {
    let (tx, rx) = mpsc::unbounded_channel();
    tokio::spawn(async move {
        loop {
            if let Ok(stream) = UnixStream::connect(&path).await {
                if tx.send(Update::Connected).is_err() {
                    return;
                }
                let mut lines = BufReader::new(stream).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if let Ok(DaemonMessage::Event { event }) = serde_json::from_str(&line)
                        && tx.send(Update::Event(event)).is_err()
                    {
                        return;
                    }
                }
                if tx.send(Update::Disconnected).is_err() {
                    return;
                }
            }
            if tx.is_closed() {
                return;
            }
            tokio::time::sleep(RETRY).await;
        }
    });
    rx
}

/// Why a request failed.
#[derive(Debug, thiserror::Error)]
pub enum RequestError {
    #[error("the daemon isn't running")]
    NotRunning,
    #[error("the connection misbehaved: {0}")]
    Protocol(String),
}

/// Send one command and wait for its response, skipping events.
pub async fn request(cmd: Command) -> Result<Response, RequestError> {
    request_at(&socket_path(), cmd).await
}

/// [`request`] at a given socket path (tests).
pub async fn request_at(path: &std::path::Path, cmd: Command) -> Result<Response, RequestError> {
    let stream = UnixStream::connect(path)
        .await
        .map_err(|_| RequestError::NotRunning)?;
    let (read, mut write) = stream.into_split();
    let mut line = serde_json::to_string(&Request { id: 1, cmd })
        .map_err(|e| RequestError::Protocol(e.to_string()))?;
    line.push('\n');
    write
        .write_all(line.as_bytes())
        .await
        .map_err(|e| RequestError::Protocol(e.to_string()))?;
    let mut lines = BufReader::new(read).lines();
    loop {
        let line = lines
            .next_line()
            .await
            .map_err(|e| RequestError::Protocol(e.to_string()))?
            .ok_or_else(|| RequestError::Protocol("closed before responding".into()))?;
        match serde_json::from_str::<DaemonMessage>(&line) {
            Ok(DaemonMessage::Response { response, .. }) => return Ok(response),
            Ok(DaemonMessage::Event { .. }) => {}
            Err(e) => return Err(RequestError::Protocol(e.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::State;
    use tokio::net::UnixListener;

    fn sock(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("cosmo-ipc-{name}-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&p);
        p
    }

    async fn serve_one(listener: &UnixListener, events: &[Event]) {
        let (mut s, _) = listener.accept().await.unwrap();
        for e in events {
            let msg = DaemonMessage::Event { event: e.clone() };
            let line = serde_json::to_string(&msg).unwrap() + "\n";
            s.write_all(line.as_bytes()).await.unwrap();
        }
        // Dropped: the daemon "restarts".
    }

    #[tokio::test]
    async fn a_subscription_survives_a_daemon_restart() {
        let path = sock("restart");
        // Subscribed before the daemon exists: it waits.
        let mut rx = subscribe_at(path.clone());
        tokio::time::sleep(Duration::from_millis(50)).await;
        let listener = UnixListener::bind(&path).unwrap();
        let idle = Event::State { state: State::Idle };
        let listening = Event::State {
            state: State::Listening,
        };
        serve_one(&listener, std::slice::from_ref(&idle)).await;
        serve_one(&listener, std::slice::from_ref(&listening)).await;

        let mut seen = Vec::new();
        while seen.len() < 6 {
            let u = tokio::time::timeout(Duration::from_secs(5), rx.recv())
                .await
                .unwrap()
                .unwrap();
            seen.push(match u {
                Update::Connected => "connected".to_owned(),
                Update::Disconnected => "disconnected".to_owned(),
                Update::Event(Event::State { state }) => format!("{state:?}"),
                Update::Event(e) => format!("{e:?}"),
            });
        }
        assert_eq!(
            seen,
            [
                "connected",
                "Idle",
                "disconnected",
                "connected",
                "Listening",
                "disconnected"
            ]
        );
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn a_request_skips_events_and_returns_its_response() {
        let path = sock("request");
        let listener = UnixListener::bind(&path).unwrap();
        tokio::spawn(async move {
            let (s, _) = listener.accept().await.unwrap();
            let (read, mut write) = s.into_split();
            let mut lines = BufReader::new(read).lines();
            let req: Request =
                serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
            assert!(matches!(req.cmd, Command::Confirm { ref token } if token == "abc"));
            for msg in [
                DaemonMessage::Event {
                    event: Event::State {
                        state: State::Acting,
                    },
                },
                DaemonMessage::Response {
                    id: req.id,
                    response: Response::Toggled { paused: false },
                },
            ] {
                let line = serde_json::to_string(&msg).unwrap() + "\n";
                write.write_all(line.as_bytes()).await.unwrap();
            }
        });
        let r = request_at(
            &path,
            Command::Confirm {
                token: "abc".into(),
            },
        )
        .await
        .unwrap();
        assert!(matches!(r, Response::Toggled { paused: false }));
        assert!(matches!(
            request_at(&sock("absent"), Command::Status).await,
            Err(RequestError::NotRunning)
        ));
        let _ = std::fs::remove_file(&path);
    }
}
