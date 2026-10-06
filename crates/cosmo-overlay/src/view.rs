//! The overlay's view model (phase-6 spec §6.1): daemon events in, what to
//! show out. Pure, so every state is testable without a compositor.
//!
//! [`View::apply`] reports whether anything *visible* changed. That's the
//! first half of the redraw discipline: a burst of events that changes
//! nothing on screen costs no frame, and the surface redraws at most once
//! per burst.

use cosmo_ipc::{Event, State};

/// How many level readings the waveform shows (newest last).
pub const WAVE_POINTS: usize = 48;

/// A held action waiting on the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hold {
    pub token: String,
    /// In plain words.
    pub action: String,
}

/// Everything the overlay draws.
#[derive(Debug, Clone, PartialEq)]
pub struct View {
    pub connected: bool,
    pub state: State,
    /// The live transcript: the streaming partial while listening, then the
    /// committed text.
    pub transcript: String,
    /// What cosmo is doing, in plain words, while acting.
    pub action: Option<String>,
    /// Actions awaiting a confirm, oldest first.
    pub holds: Vec<Hold>,
    /// The reply being spoken.
    pub reply: String,
    /// Recent mic levels for the waveform, 0–1, newest last.
    pub levels: Vec<f32>,
    /// A voice's phrases rendering: (voice, done, total).
    pub voice_render: Option<(String, u32, u32)>,
}

impl Default for View {
    fn default() -> Self {
        Self {
            connected: false,
            state: State::Idle,
            transcript: String::new(),
            action: None,
            holds: Vec::new(),
            reply: String::new(),
            levels: vec![0.0; WAVE_POINTS],
            voice_render: None,
        }
    }
}

impl View {
    /// Whether the overlay should be on screen at all. Idle with nothing
    /// pending is no overlay: the face appears only when there's something
    /// to see.
    pub fn visible(&self) -> bool {
        self.connected
            && (self.state != State::Idle || !self.holds.is_empty() || self.voice_render.is_some())
    }

    /// The daemon went away (or came back): the overlay shows nothing while
    /// it's gone, rather than a stale state.
    pub fn set_connected(&mut self, connected: bool) -> bool {
        if self.connected == connected {
            return false;
        }
        *self = Self {
            connected,
            ..Self::default()
        };
        true
    }

    /// Take one event. Returns whether anything visible changed.
    pub fn apply(&mut self, event: &Event) -> bool {
        let before = self.clone();
        match event {
            Event::State { state } => {
                // A new recording starts with a clean slate.
                if *state == State::Listening && self.state != State::Listening {
                    self.transcript.clear();
                    self.reply.clear();
                    self.action = None;
                    self.levels = vec![0.0; WAVE_POINTS];
                }
                if *state == State::Idle {
                    self.action = None;
                }
                self.state = *state;
            }
            Event::Transcript { text, .. } => self.transcript.clone_from(text),
            Event::Level { rms } => {
                self.levels.remove(0);
                self.levels.push(level(*rms));
            }
            Event::ToolStarted { tool, args, .. } => {
                self.action = Some(plain_words(tool, args));
            }
            Event::ToolFinished { .. } => {}
            Event::Held { token, action } => {
                if !self.holds.iter().any(|h| &h.token == token) {
                    self.holds.push(Hold {
                        token: token.clone(),
                        action: plain_words(action, ""),
                    });
                }
            }
            Event::HoldResolved { token, .. } => self.holds.retain(|h| &h.token != token),
            Event::Reply { text } => self.reply.clone_from(text),
            Event::VoiceCacheProgress {
                voice, done, total, ..
            } => self.voice_render = Some((voice.clone(), *done, *total)),
            Event::VoiceCacheDone { .. } => self.voice_render = None,
            // Sign-in outcomes are the applet's to show.
            Event::Usage { .. } | Event::Log { .. } | Event::SignIn { .. } => {}
        }
        // Levels only matter while listening: off-screen, they don't redraw.
        if self.state != State::Listening && self.levels != before.levels {
            return self.visible_part() != before.visible_part();
        }
        *self != before
    }

    /// Correct the view from the daemon's own status: events can be
    /// missed (a client that falls behind has some skipped), and a missed
    /// "idle" or "resolved" would leave the card up for good. Returns
    /// whether anything visible changed.
    pub fn reconcile(&mut self, status: &cosmo_ipc::StatusInfo) -> bool {
        let before = self.clone();
        // Mid-listen the events are authoritative (and ~20 a second).
        if self.state != State::Listening || status.state != State::Idle {
            self.state = status.state;
        }
        self.holds
            .retain(|h| status.pending_holds.iter().any(|p| p.token == h.token));
        for p in &status.pending_holds {
            if !self.holds.iter().any(|h| h.token == p.token) {
                self.holds.push(Hold {
                    token: p.token.clone(),
                    action: p.action.clone(),
                });
            }
        }
        if self.state == State::Idle {
            self.action = None;
        }
        self.visible_part() != before.visible_part()
    }

    /// Everything but the waveform, for the check above.
    fn visible_part(&self) -> (State, &str, &Option<String>, &[Hold], &str, bool) {
        (
            self.state,
            &self.transcript,
            &self.action,
            &self.holds,
            &self.reply,
            self.visible(),
        )
    }
}

/// One burst of subscription updates, applied together (the first half of
/// the redraw discipline): the overlay hears about a burst **once**, and
/// only if something visible changed. Returns whether it did.
pub fn apply_burst(
    view: &mut View,
    burst: impl IntoIterator<Item = cosmo_ipc::client::Update>,
) -> bool {
    use cosmo_ipc::client::Update;
    let mut changed = false;
    for update in burst {
        changed |= match update {
            Update::Connected => view.set_connected(true),
            Update::Disconnected => view.set_connected(false),
            Update::Event(e) => view.apply(&e),
        };
    }
    changed
}

/// A mic RMS as a 0–1 bar height. Speech sits around 0.01–0.2 RMS, so the
/// scale is logarithmic: −60 dBFS is empty, −10 dBFS is full.
pub fn level(rms: f32) -> f32 {
    if rms <= 0.0 {
        return 0.0;
    }
    let db = 20.0 * rms.log10();
    ((db + 60.0) / 50.0).clamp(0.0, 1.0)
}

/// A tool call as the user would say it. Unknown tools fall back to their
/// name with spaces.
pub fn plain_words(tool: &str, args: &str) -> String {
    let args: serde_json::Value = serde_json::from_str(args).unwrap_or_default();
    let arg = |k: &str| args[k].as_str().map(str::to_owned);
    match tool {
        "launch_app" => format!("Opening {}", arg("app").map_or("an app".into(), pretty_app)),
        "focus_app" => format!(
            "Switching to {}",
            arg("app").map_or("an app".into(), pretty_app)
        ),
        "media_control" => match arg("command").as_deref() {
            Some("pause") => "Pausing".into(),
            Some("play") => "Playing".into(),
            Some("next") => "Next track".into(),
            Some("previous") => "Previous track".into(),
            Some("stop") => "Stopping playback".into(),
            _ => "Controlling playback".into(),
        },
        "switch_workspace" => match args["workspace"].as_u64() {
            Some(n) => format!("Going to workspace {n}"),
            None => "Switching workspace".into(),
        },
        "move_window_to_workspace" => match args["workspace"].as_u64() {
            Some(n) => format!("Moving the window to workspace {n}"),
            None => "Moving the window".into(),
        },
        "maximize_window" => "Maximizing the window".into(),
        "minimize_window" => "Minimizing the window".into(),
        "run_in_terminal" => "Running a command".into(),
        "read_terminal" | "watch_terminal" => "Checking the terminal".into(),
        "list_windows" | "get_accessibility_tree" | "get_app_state" => {
            "Looking at your windows".into()
        }
        "screenshot" => "Taking a screenshot".into(),
        "click" | "double_click" | "right_click" => "Clicking".into(),
        "type_text" | "dictate" => "Typing".into(),
        "press_key" => "Pressing a key".into(),
        "scroll" => "Scrolling".into(),
        "system_query" => "Checking the system".into(),
        "remember" => "Remembering that".into(),
        "clipboard_get" => "Reading the clipboard".into(),
        "clipboard_set" => "Copying to the clipboard".into(),
        other => {
            let mut s = other.replace('_', " ");
            if let Some(c) = s.get_mut(0..1) {
                c.make_ascii_uppercase();
            }
            s
        }
    }
}

/// `org.mozilla.firefox` → `Firefox`: ids are what reflex sends.
fn pretty_app(id: String) -> String {
    let last = id
        .rsplit('.')
        .next()
        .unwrap_or(&id)
        .replace(['-', '_'], " ");
    let mut s = last;
    if let Some(c) = s.get_mut(0..1) {
        c.make_ascii_uppercase();
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connected() -> View {
        let mut v = View::default();
        v.set_connected(true);
        v
    }

    fn state(s: State) -> Event {
        Event::State { state: s }
    }

    #[test]
    fn idle_is_invisible_and_listening_is_not() {
        let mut v = connected();
        assert!(!v.visible());
        assert!(v.apply(&state(State::Listening)));
        assert!(v.visible());
        assert!(v.apply(&state(State::Idle)));
        assert!(!v.visible());
        assert!(!View::default().visible(), "never while disconnected");
    }

    #[test]
    fn a_recording_shows_partials_then_the_commit() {
        let mut v = connected();
        v.apply(&state(State::Listening));
        let partial = |t: &str| Event::Transcript {
            text: t.into(),
            r#final: false,
            latency_ms: None,
        };
        assert!(v.apply(&partial("Open")));
        assert!(v.apply(&partial("Open Fire")));
        assert!(
            !v.apply(&partial("Open Fire")),
            "the same text is no change"
        );
        assert_eq!(v.transcript, "Open Fire");
        v.apply(&Event::Transcript {
            text: "Open Firefox.".into(),
            r#final: true,
            latency_ms: Some(80),
        });
        assert_eq!(v.transcript, "Open Firefox.");
        // The next recording starts clean.
        v.apply(&state(State::Idle));
        v.apply(&state(State::Listening));
        assert_eq!(v.transcript, "");
    }

    #[test]
    fn actions_read_as_plain_words() {
        let mut v = connected();
        v.apply(&state(State::Acting));
        v.apply(&Event::ToolStarted {
            call_id: "reflex-1".into(),
            tool: "launch_app".into(),
            args: r#"{"app":"org.mozilla.firefox"}"#.into(),
        });
        assert_eq!(v.action.as_deref(), Some("Opening Firefox"));
        assert_eq!(
            plain_words("switch_workspace", r#"{"workspace":2}"#),
            "Going to workspace 2"
        );
        assert_eq!(
            plain_words("media_control", r#"{"command":"pause"}"#),
            "Pausing"
        );
        assert_eq!(plain_words("some_new_tool", ""), "Some new tool");
        v.apply(&state(State::Idle));
        assert_eq!(v.action, None);
    }

    #[test]
    fn holds_stay_visible_until_resolved_even_when_idle() {
        let mut v = connected();
        v.apply(&Event::Held {
            token: "38e1b3fb".into(),
            action: "run_in_terminal".into(),
        });
        v.apply(&state(State::Idle));
        assert!(v.visible(), "a pending confirm keeps the overlay up");
        assert_eq!(
            v.holds,
            [Hold {
                token: "38e1b3fb".into(),
                action: "Running a command".into()
            }]
        );
        assert!(v.apply(&Event::HoldResolved {
            token: "38e1b3fb".into(),
            executed: true,
            summary: "ok".into(),
        }));
        assert!(!v.visible());
    }

    #[test]
    fn levels_redraw_only_while_listening() {
        let mut v = connected();
        v.apply(&state(State::Listening));
        assert!(v.apply(&Event::Level { rms: 0.1 }));
        assert!(*v.levels.last().unwrap() > 0.5);
        v.apply(&state(State::Thinking));
        assert!(
            !v.apply(&Event::Level { rms: 0.1 }),
            "no frame for an off-screen waveform"
        );
    }

    #[test]
    fn events_that_show_nothing_cost_nothing() {
        let mut v = connected();
        assert!(!v.apply(&Event::Log { line: "x".into() }));
        assert!(!v.apply(&Event::Usage {
            prompt_tokens: 1,
            completion_tokens: 1,
            total_tokens: 2,
            remaining_requests: None,
            remaining_tokens: None,
        }));
        assert!(!v.apply(&state(State::Idle)), "already idle");
    }

    #[test]
    fn a_daemon_restart_clears_everything() {
        let mut v = connected();
        v.apply(&state(State::Speaking));
        assert!(v.set_connected(false));
        assert!(!v.visible());
        assert!(v.set_connected(true));
        assert_eq!(v.state, State::Idle);
    }

    /// A reflex command's whole burst (state, tool events, transcript,
    /// back to idle) arriving together is one change, not six frames, and
    /// a burst of invisible events is none.
    #[test]
    fn a_burst_is_one_change_and_an_invisible_burst_is_none() {
        use cosmo_ipc::client::Update;
        let mut v = connected();
        let burst = vec![
            Update::Event(state(State::Acting)),
            Update::Event(Event::ToolStarted {
                call_id: "reflex-1".into(),
                tool: "launch_app".into(),
                args: r#"{"app":"firefox"}"#.into(),
            }),
            Update::Event(Event::ToolFinished {
                call_id: "reflex-1".into(),
                tool: "launch_app".into(),
                ok: true,
                latency_ms: 3,
                summary: "started Firefox".into(),
            }),
        ];
        assert!(apply_burst(&mut v, burst));
        assert_eq!(v.action.as_deref(), Some("Opening Firefox"));
        let noise = vec![
            Update::Event(Event::Log { line: "x".into() }),
            Update::Event(Event::Level { rms: 0.2 }), // not listening
        ];
        assert!(!apply_burst(&mut v, noise));
        // Idle and back within one burst: net, nothing new to draw, but the
        // state did change, so it counts.
        assert!(apply_burst(&mut v, vec![Update::Event(state(State::Idle))]));
    }

    #[test]
    fn the_level_scale_is_logarithmic() {
        assert_eq!(level(0.0), 0.0);
        assert_eq!(level(0.001), 0.0); // −60 dBFS
        assert!((level(0.316) - 1.0).abs() < 0.01); // −10 dBFS
        assert!(level(0.03) > 0.3 && level(0.03) < 0.6); // quiet speech
    }

    fn status(state: State, holds: &[&str]) -> cosmo_ipc::StatusInfo {
        cosmo_ipc::StatusInfo {
            state,
            paused: false,
            version: "0".into(),
            pending_holds: holds
                .iter()
                .map(|t| cosmo_ipc::PendingHold {
                    token: (*t).into(),
                    action: "run a command".into(),
                    parked_at_ms: 0,
                })
                .collect(),
        }
    }

    /// The bug the user hit: a missed "idle" (or "resolved") left the card
    /// on screen. The daemon's status puts it right.
    #[test]
    fn a_missed_idle_or_resolution_is_put_right() {
        let mut v = View::default();
        v.set_connected(true);
        v.apply(&Event::State {
            state: State::Thinking,
        });
        v.apply(&Event::Held {
            token: "t1".into(),
            action: "click".into(),
        });
        assert!(v.visible());
        assert!(v.reconcile(&status(State::Idle, &[])));
        assert!(!v.visible(), "nothing pending, idle: the card goes");
        assert!(
            !v.reconcile(&status(State::Idle, &[])),
            "no change, no redraw"
        );
        // A hold the overlay never heard about appears.
        assert!(v.reconcile(&status(State::Waiting, &["t2"])));
        assert_eq!(v.holds.len(), 1);
    }
}
