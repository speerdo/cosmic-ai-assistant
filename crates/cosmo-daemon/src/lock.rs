//! Lock state on COSMIC (invariant #10), from two things a normal client
//! can see. Measured on 2026-10-05 (phase-8 findings §8):
//!
//! - **logind's `Session.Lock` signal** (and `PrepareForSleep`): what
//!   COSMIC's locker, cosmic-greeter, locks on. It fired at the instant
//!   the lock screen appeared. No `Unlock` signal ever comes: the greeter
//!   unlocks after PAM without telling logind, and nobody sets
//!   `LockedHint` (findings §L).
//! - **The compositor's activated window** (`cosmo-focus`): ~0.5 s after
//!   the lock, no window was activated; ~0.4 s after the unlock, the
//!   previous one was again.
//!
//! The rule, fail-closed at every step:
//! - a `Lock` (or sleep) signal ⇒ **Locked**, until every window has been
//!   seen inactive *and then* one active again (an activation left over
//!   from before the lock doesn't count);
//! - otherwise a window is active ⇒ **Unlocked**;
//! - no window active ⇒ **Unknown** (refuse): locked, or simply nothing
//!   focused, and the two can't be told apart;
//! - the window connection fails ⇒ **Unknown**.
//!
//! If the signal subscription can't be made, this isn't used at all and
//! the old deny-everything policy stands.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use cosmo_gate::{Gate, LockState};
use futures::StreamExt;

/// How often the activated window is read: a ~1 ms round trip on a
/// persistent connection.
const POLL: Duration = Duration::from_millis(100);

/// The rule above, with no I/O.
#[derive(Debug, Default)]
pub struct Tracker {
    /// A lock signal arrived and hasn't been cleared by a fresh activation.
    pending_lock: bool,
    /// Since that signal, a moment with no active window was seen.
    saw_inactive: bool,
    /// The last window reading: `Some(true)` = a window is active.
    active: Option<bool>,
    /// logind's signals stopped arriving: a lock could go unseen, so
    /// nothing is trusted any more.
    signals_lost: bool,
}

impl Tracker {
    /// logind said Lock (or the machine is about to sleep).
    pub fn lock_signal(&mut self) -> LockState {
        self.pending_lock = true;
        self.saw_inactive = false;
        self.state()
    }

    /// A window reading: `Ok(true)` some window is activated, `Ok(false)`
    /// none is, `Err` the compositor couldn't be asked.
    pub fn windows(&mut self, reading: Result<bool, ()>) -> LockState {
        self.active = reading.ok();
        match self.active {
            Some(false) if self.pending_lock => self.saw_inactive = true,
            Some(true) if self.pending_lock && self.saw_inactive => {
                self.pending_lock = false;
                self.saw_inactive = false;
            }
            _ => {}
        }
        self.state()
    }

    /// logind's signal stream ended: fail closed from now on.
    pub fn lose_signals(&mut self) -> LockState {
        self.signals_lost = true;
        self.state()
    }

    pub fn state(&self) -> LockState {
        if self.signals_lost {
            return LockState::Unknown;
        }
        match (self.pending_lock, self.active) {
            (true, _) => LockState::Locked,
            (false, Some(true)) => LockState::Unlocked,
            (false, Some(false) | None) => LockState::Unknown,
        }
    }

    /// For `doctor`: what the current state means.
    pub fn describe(&self) -> &'static str {
        if self.signals_lost {
            return "logind's lock signals stopped arriving: screen tools refuse until cosmo \
                    restarts";
        }
        match (self.pending_lock, self.active) {
            (true, _) => "the session is locked: screen tools refuse until you unlock",
            (false, Some(true)) => {
                "unlocked (a window is active, and no lock since): screen tools available"
            }
            (false, Some(false)) => {
                "no window is active, so screen tools refuse until one is (open or \
                 activate an app; the lock screen looks the same from here)"
            }
            (false, None) => "the compositor can't be asked: screen tools refuse",
        }
    }
}

/// This user's graphical session's logind object path. A systemd user
/// service has no `XDG_SESSION_ID` (it runs outside the session), so it
/// falls back to logind's `User.Display`: the session holding the user's
/// display.
pub async fn session_path(conn: &zbus::Connection) -> anyhow::Result<String> {
    match display_session(conn).await {
        Ok(path) => Ok(path),
        Err(e) => match std::env::var("XDG_SESSION_ID")
            .ok()
            .filter(|s| !s.is_empty())
        {
            Some(id) => Ok(format!("/org/freedesktop/login1/session/{}", escape(&id))),
            None => Err(e),
        },
    }
}

/// logind's `User.Display` for this uid: the session holding its display.
async fn display_session(conn: &zbus::Connection) -> anyhow::Result<String> {
    // The runtime dir is /run/user/<uid>: the uid without `unsafe`.
    let uid = std::env::var("XDG_RUNTIME_DIR")
        .ok()
        .and_then(|d| d.rsplit('/').next().and_then(|u| u.parse::<u32>().ok()))
        .ok_or_else(|| anyhow::anyhow!("can't tell this user's uid"))?;
    let props = zbus::fdo::PropertiesProxy::builder(conn)
        .destination("org.freedesktop.login1")?
        .path(format!("/org/freedesktop/login1/user/_{uid}"))?
        .build()
        .await?;
    let display = props
        .get("org.freedesktop.login1.User".try_into()?, "Display")
        .await?;
    let (id, path): (String, zbus::zvariant::OwnedObjectPath) = display.try_into()?;
    if id.is_empty() {
        anyhow::bail!("logind reports no graphical session for uid {uid}");
    }
    Ok(path.to_string())
}

/// A D-Bus object path element, escaped as systemd does: anything but an
/// ASCII letter or digit, and a leading digit, becomes `_` and two hex
/// digits (session "3" is `_33`).
fn escape(id: &str) -> String {
    id.bytes()
        .enumerate()
        .map(|(i, b)| {
            if b.is_ascii_alphabetic() || (b.is_ascii_digit() && i > 0) {
                (b as char).to_string()
            } else {
                format!("_{b:02x}")
            }
        })
        .collect()
}

/// Track COSMIC's lock state into `gate` from now on. `Err` when logind's
/// signals can't be subscribed to: then nothing is tracked, and the caller
/// keeps denying.
pub async fn start(gate: Arc<Gate>) -> anyhow::Result<Arc<Mutex<Tracker>>> {
    let conn = zbus::Connection::system().await?;
    let session_path = session_path(&conn).await?;
    let lock_rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender("org.freedesktop.login1")?
        .path(session_path)?
        .interface("org.freedesktop.login1.Session")?
        .member("Lock")?
        .build();
    let sleep_rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender("org.freedesktop.login1")?
        .path("/org/freedesktop/login1")?
        .interface("org.freedesktop.login1.Manager")?
        .member("PrepareForSleep")?
        .build();
    let mut locks = zbus::MessageStream::for_match_rule(lock_rule, &conn, None).await?;
    let mut sleeps = zbus::MessageStream::for_match_rule(sleep_rule, &conn, None).await?;

    let tracker = Arc::new(Mutex::new(Tracker::default()));
    gate.set_lock_state(LockState::Unknown);

    // logind: a lock (or an imminent sleep) locks at once.
    {
        let (tracker, gate) = (Arc::clone(&tracker), Arc::clone(&gate));
        tokio::spawn(async move {
            let _conn = conn; // the streams need it alive
            loop {
                let start_sleep = tokio::select! {
                    m = locks.next() => m.map(|_| true),
                    m = sleeps.next() => m.map(|m| {
                        m.ok()
                            .and_then(|m| m.body().deserialize::<bool>().ok())
                            .unwrap_or(true)
                    }),
                };
                // The gate is set with the tracker held, so this and the
                // window thread can't overwrite each other out of order.
                match start_sleep {
                    // A Lock, or sleep starting: locked.
                    Some(true) => {
                        let mut t = tracker.lock().unwrap();
                        gate.set_lock_state(t.lock_signal());
                        tracing::info!("logind lock/sleep signal: screen tools refuse");
                    }
                    // Resume from sleep: the window reading decides.
                    Some(false) => {}
                    None => {
                        let mut t = tracker.lock().unwrap();
                        gate.set_lock_state(t.lose_signals());
                        tracing::warn!("logind signal stream ended; screen tools refuse");
                        return;
                    }
                }
            }
        });
    }

    // The compositor: is any window activated? A thread of its own: the
    // window connection is blocking.
    {
        let (tracker, gate) = (Arc::clone(&tracker), Arc::clone(&gate));
        std::thread::Builder::new()
            .name("cosmo-lock-windows".into())
            .spawn(move || {
                let mut windows = cosmo_focus::control::WindowService::start().ok();
                let mut last = None;
                loop {
                    let reading = match &windows {
                        Some(w) => w.snapshot().map(|s| s.focused().is_some()).map_err(|_| ()),
                        None => Err(()),
                    };
                    if reading.is_err() {
                        // Reconnect next time round (the compositor restarted?).
                        windows = cosmo_focus::control::WindowService::start().ok();
                    }
                    let state = {
                        let mut t = tracker.lock().unwrap();
                        let state = t.windows(reading);
                        gate.set_lock_state(state);
                        state
                    };
                    if last != Some(state) {
                        tracing::info!(?state, "lock state");
                        last = Some(state);
                    }
                    std::thread::sleep(POLL);
                }
            })?;
    }
    Ok(tracker)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The sequence measured on 2026-10-05: active window, Lock signal,
    /// windows inactive, (PAM), a window active again.
    #[test]
    fn the_measured_lock_and_unlock() {
        let mut t = Tracker::default();
        assert_eq!(t.state(), LockState::Unknown, "nothing seen yet: refuse");
        assert_eq!(t.windows(Ok(true)), LockState::Unlocked);
        assert_eq!(t.lock_signal(), LockState::Locked);
        // The window is still active for ~0.5 s after the signal: that
        // activation is from before the lock and doesn't unlock.
        assert_eq!(t.windows(Ok(true)), LockState::Locked);
        assert_eq!(t.windows(Ok(false)), LockState::Locked);
        assert_eq!(t.windows(Ok(false)), LockState::Locked);
        // Unlocked: a window is active again.
        assert_eq!(t.windows(Ok(true)), LockState::Unlocked);
    }

    #[test]
    fn no_active_window_or_no_compositor_refuses() {
        let mut t = Tracker::default();
        assert_eq!(t.windows(Ok(false)), LockState::Unknown);
        assert_eq!(t.windows(Err(())), LockState::Unknown);
        assert_eq!(t.windows(Ok(true)), LockState::Unlocked);
        assert_eq!(t.windows(Err(())), LockState::Unknown);
    }

    #[test]
    fn a_lock_signal_holds_through_a_failed_reading() {
        let mut t = Tracker::default();
        t.windows(Ok(true));
        t.lock_signal();
        assert_eq!(t.windows(Err(())), LockState::Locked);
        // An error isn't "inactive": it doesn't count towards unlocking.
        assert_eq!(t.windows(Ok(true)), LockState::Locked);
        t.windows(Ok(false));
        assert_eq!(t.windows(Ok(true)), LockState::Unlocked);
    }

    #[test]
    fn a_second_lock_before_unlock_starts_over() {
        let mut t = Tracker::default();
        t.windows(Ok(true));
        t.lock_signal();
        t.windows(Ok(false));
        t.lock_signal();
        assert_eq!(t.windows(Ok(true)), LockState::Locked);
    }

    #[test]
    fn session_ids_escape_like_systemd() {
        assert_eq!(escape("3"), "_33");
        assert_eq!(escape("c2"), "c2");
        assert_eq!(escape("12"), "_312");
    }

    #[test]
    fn lost_signals_never_recover() {
        let mut t = Tracker::default();
        t.windows(Ok(true));
        assert_eq!(t.lose_signals(), LockState::Unknown);
        t.windows(Ok(false));
        assert_eq!(t.windows(Ok(true)), LockState::Unknown);
    }
}
