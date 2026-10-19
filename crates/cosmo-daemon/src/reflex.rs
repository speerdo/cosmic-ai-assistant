//! The reflex path in the daemon (phase-4 spec §4.2, §4.4): matched safe
//! verbs, carried out locally with no model.
//!
//! [`Actuator`] is what carries an [`Intent`] out. The daemon's is
//! [`Desktop`] (MPRIS, `.desktop` launch, COSMIC window protocols); tests use
//! a recording fake, so the engine's reflex path is testable without
//! touching a real desktop.

use std::sync::{Arc, Mutex};

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
        Self::new(
            Matcher::new(apps),
            Arc::new(Desktop {
                available: windows.is_some(),
                windows: Mutex::new(windows.map(Arc::new)),
            }),
        )
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
    /// The compositor offered the protocols at startup. If not, there is
    /// nothing to restart.
    available: bool,
    windows: Mutex<Option<Arc<WindowService>>>,
}

impl Desktop {
    /// The running window service, started again if its thread has died
    /// (the compositor connection was lost).
    fn windows(&self) -> Result<Arc<WindowService>, String> {
        if !self.available {
            return Err("no window control on this compositor".to_owned());
        }
        let mut slot = self.windows.lock().unwrap();
        if let Some(w) = slot.as_ref().filter(|w| w.is_alive()) {
            return Ok(Arc::clone(w));
        }
        tracing::info!("window service stopped; restarting it");
        let w = Arc::new(WindowService::start().map_err(|e| e.to_string())?);
        *slot = Some(Arc::clone(&w));
        Ok(w)
    }

    /// Run a window verb. If the service died under it, start a new one and
    /// try once more. Other failures (no such window) are the verb's answer.
    fn window_call<R>(&self, f: impl Fn(&WindowService) -> anyhow::Result<R>) -> Result<R, String> {
        let w = self.windows()?;
        match f(&w) {
            Err(_) if !w.is_alive() => {
                drop(w);
                f(&*self.windows()?).map_err(|e| e.to_string())
            }
            r => r.map_err(|e| e.to_string()),
        }
    }
}

impl Actuator for Desktop {
    fn act<'a>(&'a self, intent: &'a Intent) -> BoxFuture<'a, Result<String, String>> {
        Box::pin(async move {
            match intent {
                Intent::Media(cmd) => cosmo_tools::media::control(cmd.as_str())
                    .await
                    .map_err(|e| e.to_string()),
                Intent::Ask(cosmo_reflex::Ask::Time) => Ok(cosmo_tools::clock::time_line()),
                Intent::Ask(cosmo_reflex::Ask::Date) => Ok(cosmo_tools::clock::date_line()),
                Intent::Volume(cmd) => {
                    let (action, amount) = cmd.parts();
                    let change = cosmo_tools::volume::Change::parse(action, amount.map(u64::from))
                        .ok_or("not a volume change")?;
                    cosmo_tools::volume::apply(change)
                        .await
                        .map_err(|e| e.to_string())
                }
                Intent::Launch(app) => {
                    cosmo_tools::launch::launch(&app.id).map_err(|e| e.to_string())
                }
                Intent::LaunchOnNewWorkspace(app) => {
                    self.window_call(|w| w.switch_new_workspace())?;
                    cosmo_tools::launch::launch(&app.id).map_err(|e| e.to_string())
                }
                Intent::Focus(app) => {
                    let ids = window_ids(&app.id);
                    self.window_call(|w| w.focus_app(&ids))
                        .map(|title| format!("focused {title}"))
                }
                Intent::SwitchWorkspace(n) => self
                    .window_call(|w| w.switch_workspace(*n))
                    .map(|()| format!("workspace {n}")),
                Intent::MoveToWorkspace(n) => self
                    .window_call(|w| w.move_focused_to_workspace(*n))
                    .map(|()| format!("moved to workspace {n}")),
                Intent::Maximize => self
                    .window_call(|w| w.maximize_focused())
                    .map(|()| "maximized".into()),
                Intent::Minimize => self
                    .window_call(|w| w.minimize_focused())
                    .map(|()| "minimized".into()),
            }
        })
    }
}
