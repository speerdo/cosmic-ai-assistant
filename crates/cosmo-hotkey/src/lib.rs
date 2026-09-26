//! The hold-to-talk trigger, read straight off evdev (phase-3 spec §3.4).
//!
//! cosmic-comp's shortcuts spawn a process on key *press* only, and the
//! portal has no GlobalShortcuts, so neither can report the *release* a
//! hold-to-talk trigger needs (blueprint §3.1). We read the keyboard nodes.
//!
//! ## Invariants
//!
//! - **`EVIOCSMASK` restricts each descriptor to the trigger keycode**, with
//!   `EV_MSC` (scancodes) masked off entirely. The kernel never copies any
//!   other keystroke into this process: it is not a keylogger, mechanically.
//! - **No `EVIOCGRAB`.** The blueprint grabbed the device while the trigger
//!   was held; phase 3 found that design unsound (findings §3). A grab can
//!   only start *after* the press has reached the compositor, and it then
//!   withholds the release — so the compositor believes the key is still
//!   down. For a modifier trigger (Right Ctrl, the chosen default) that is a
//!   stuck Ctrl turning later typing into shortcuts. Instead the trigger is
//!   a key inert on its own, and its press and release flow normally.
//! - No root, no `input` group: logind's `uaccess` ACL on the node suffices.
//! - Autorepeat (`value == 2`) is ignored; only edges count.
//! - Hotplug by inotify on `/dev/input` (no libudev): a replugged keyboard
//!   is re-attached, and a device vanishing mid-hold counts as a release.
//! - Every keyboard that can emit the trigger is watched; the trigger is
//!   "held" while any of them holds it.

mod state;

#[allow(unsafe_code)] // the ioctls are the point; all unsafe lives here
mod evdev;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

pub use state::TriggerState;

/// `KEY_RIGHTCTRL`: the default trigger (chosen 2026-09-26, findings §3).
pub const KEY_RIGHTCTRL: u16 = 97;

/// A trigger edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Pressed,
    Released,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TriggerEvent {
    pub edge: Edge,
    /// When the edge was read (monotonic), for pre-roll arithmetic.
    pub at: Instant,
}

/// The running watcher. Dropping it stops the thread.
pub struct Watcher {
    devices: Arc<Mutex<Vec<PathBuf>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for Watcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Watcher")
            .field("devices", &self.devices())
            .finish_non_exhaustive()
    }
}

impl Watcher {
    /// Watch `code` on every keyboard that has it, now and as they come and
    /// go, calling `on_edge` for each press/release of the combined state.
    /// `on_edge` runs on the watcher thread and must not block for long.
    pub fn start(
        code: u16,
        on_edge: impl Fn(TriggerEvent) + Send + 'static,
    ) -> std::io::Result<Self> {
        let devices = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let inotify = evdev::Hotplug::new()?;
        let (t_devices, t_stop) = (devices.clone(), stop.clone());
        let thread = std::thread::Builder::new()
            .name("cosmo-hotkey".into())
            .spawn(move || run(code, inotify, &t_devices, &t_stop, on_edge))?;
        Ok(Self {
            devices,
            stop,
            thread: Some(thread),
        })
    }

    /// Device nodes currently watched (for `doctor`).
    pub fn devices(&self) -> Vec<PathBuf> {
        self.devices
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// How often the loop wakes with nothing to read, to notice `stop`.
const TICK: Duration = Duration::from_millis(250);

fn run(
    code: u16,
    hotplug: evdev::Hotplug,
    listing: &Mutex<Vec<PathBuf>>,
    stop: &AtomicBool,
    on_edge: impl Fn(TriggerEvent),
) {
    let mut devices: Vec<evdev::Device> = Vec::new();
    let mut state = TriggerState::default();
    let mut rescan = true;
    while !stop.load(Ordering::Relaxed) {
        if rescan {
            rescan = false;
            for path in evdev::candidates() {
                if devices.iter().any(|d| d.path == path) {
                    continue;
                }
                match evdev::Device::open(&path, code) {
                    Ok(Some(dev)) => {
                        tracing::info!(device = %path.display(), code, "trigger attached");
                        devices.push(dev);
                    }
                    Ok(None) => {} // cannot emit the trigger
                    // Not yet accessible (the uaccess ACL lands just after
                    // the node appears): the IN_ATTRIB that follows rescans.
                    Err(e) => {
                        tracing::debug!(device = %path.display(), error = %e, "not attachable yet")
                    }
                }
            }
            *listing.lock().unwrap_or_else(|p| p.into_inner()) =
                devices.iter().map(|d| d.path.clone()).collect();
        }

        let ready = match evdev::poll(&hotplug, &devices, TICK) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "hotkey poll failed");
                std::thread::sleep(TICK);
                continue;
            }
        };
        if ready.hotplug {
            hotplug.drain();
            rescan = true;
        }
        let mut gone = Vec::new();
        for i in ready.devices {
            match devices[i].read_edges() {
                Ok(values) => {
                    for value in values {
                        if let Some(edge) = state.on_value(devices[i].id, value) {
                            on_edge(TriggerEvent {
                                edge,
                                at: Instant::now(),
                            });
                        }
                    }
                }
                Err(e) => {
                    tracing::info!(device = %devices[i].path.display(), error = %e, "trigger device gone");
                    gone.push(i);
                }
            }
        }
        for i in gone.into_iter().rev() {
            let dev = devices.remove(i);
            // A release lost with the device must not leave cosmo listening.
            if let Some(edge) = state.forget(dev.id) {
                on_edge(TriggerEvent {
                    edge,
                    at: Instant::now(),
                });
            }
            rescan = true;
        }
    }
}
