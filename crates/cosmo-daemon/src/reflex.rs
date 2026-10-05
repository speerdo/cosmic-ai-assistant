//! The reflex path in the daemon (phase-4 spec §4.2, §4.4): matched safe
//! verbs, carried out locally with no model.
//!
//! [`Actuator`] is what carries an [`Intent`] out. The daemon's is
//! [`Desktop`] (MPRIS, `.desktop` launch, COSMIC window protocols); tests use
//! a recording fake, so the engine's reflex path is testable without
//! touching a real desktop.

use std::sync::Arc;

use cosmo_focus::control::WindowService;
use cosmo_reflex::{AppIndex, Intent, Matcher};
use futures::future::BoxFuture;

/// Carries out a reflex intent. `Err` means it didn't happen; the turn then
/// escalates to reasoning (blueprint §2: escalate rather than report
/// failure).
pub trait Actuator: Send + Sync {
    fn act<'a>(&'a self, intent: &'a Intent) -> BoxFuture<'a, Result<String, String>>;
}

/// The matcher and whatever acts on its matches.
pub struct Reflex {
    /// Shared with the reasoning tools, which resolve app names with it.
    pub matcher: Arc<Matcher>,
    pub actuator: Arc<dyn Actuator>,
}

impl Reflex {
    pub fn new(matcher: Matcher, actuator: Arc<dyn Actuator>) -> Self {
        Self {
            matcher: Arc::new(matcher),
            actuator,
        }
    }

    /// The real desktop: installed apps for the matcher, and a window
    /// service if the compositor offers the protocols (without one, window
    /// verbs fail and escalate).
    pub fn desktop() -> Self {
        let windows = match WindowService::start() {
            Ok(w) => Some(w),
            Err(e) => {
                tracing::warn!(error = %e, "window verbs unavailable; they will escalate");
                None
            }
        };
        let apps = AppIndex::installed();
        tracing::info!(
            apps = apps.len(),
            windows = windows.is_some(),
            "reflex ready"
        );
        Self::new(Matcher::new(apps), Arc::new(Desktop { windows }))
    }
}

/// What an app's windows may call themselves: its `.desktop` id, and its
/// `StartupWMClass` when it has one (`com.spotify.Client` → `spotify`).
fn window_ids(desktop_id: &str) -> Vec<String> {
    let mut ids = vec![desktop_id.to_owned()];
    if let Some(class) = cosmo_stt::hotwords::desktop_app(desktop_id).and_then(|a| a.wm_class) {
        ids.push(class);
    }
    ids
}

/// The production actuator.
pub struct Desktop {
    windows: Option<WindowService>,
}

impl Desktop {
    fn windows(&self) -> Result<&WindowService, String> {
        self.windows
            .as_ref()
            .ok_or_else(|| "no window control on this compositor".to_owned())
    }
}

impl Actuator for Desktop {
    fn act<'a>(&'a self, intent: &'a Intent) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            let e = |e: anyhow::Error| e.to_string();
            match intent {
                Intent::Media(cmd) => cosmo_tools::media::control(cmd.as_str())
                    .await
                    .map_err(|e| e.to_string()),
                Intent::Launch(app) => {
                    cosmo_tools::launch::launch(&app.id).map_err(|e| e.to_string())
                }
                Intent::Focus(app) => self
                    .windows()?
                    .focus_app(&window_ids(&app.id))
                    .map(|title| format!("focused {title}"))
                    .map_err(e),
                Intent::SwitchWorkspace(n) => self
                    .windows()?
                    .switch_workspace(*n)
                    .map(|()| format!("workspace {n}"))
                    .map_err(e),
                Intent::MoveToWorkspace(n) => self
                    .windows()?
                    .move_focused_to_workspace(*n)
                    .map(|()| format!("moved to workspace {n}"))
                    .map_err(e),
                Intent::Maximize => self
                    .windows()?
                    .maximize_focused()
                    .map(|()| "maximized".into())
                    .map_err(e),
                Intent::Minimize => self
                    .windows()?
                    .minimize_focused()
                    .map(|()| "minimized".into())
                    .map_err(e),
            }
        })
    }
}
