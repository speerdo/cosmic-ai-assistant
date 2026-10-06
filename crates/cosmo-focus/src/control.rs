//! Window verbs (phase-4 spec §4.4): focus an app, maximize or minimize the
//! focused window, move it to a workspace, switch workspace.
//!
//! COSMIC ignores virtual-keyboard modifiers for its shortcuts (`cosmo-type`),
//! so these can't be key chords. They go through the protocols the
//! compositor offers for exactly this, found live (`examples/globals.rs`):
//!
//! - `ext_foreign_toplevel_list_v1` (standard): every window's app id.
//! - `zcosmic_toplevel_info_v1` v3: each window's state (which is focused),
//!   through `get_cosmic_toplevel`.
//! - `zcosmic_toplevel_manager_v1` v4: activate, maximize, minimize,
//!   `move_to_ext_workspace`.
//! - `ext_workspace_manager_v1` (standard): workspace names, the active one,
//!   activating another.
//!
//! [`WindowService`] holds **one persistent connection** on its own thread,
//! so its picture of the windows is always current and a command costs a
//! request, not a fresh snapshot. A fresh connection measured 60–150 ms
//! before it knew which window was focused: cosmic-comp sends window state
//! on its next refresh, not in answer to a round trip. That's the whole
//! reflex budget (blueprint §5 predicted it: "hold a persistent Wayland
//! connection … and listing costs nothing"). Which window or workspace a
//! command means is decided by pure functions over the snapshot
//! ([`find_app`], [`find_workspace`]), tested without a compositor.

use std::io::{Read as _, Write as _};
use std::os::unix::net::UnixStream;
use std::sync::mpsc;

use anyhow::{Context as _, bail};
use wayland_client::backend::ObjectId;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_output, wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, event_created_child};
use wayland_protocols::ext::foreign_toplevel_list::v1::client::{
    ext_foreign_toplevel_handle_v1 as ext_top, ext_foreign_toplevel_list_v1 as ext_list,
};
use wayland_protocols::ext::workspace::v1::client::{
    ext_workspace_group_handle_v1 as ext_group, ext_workspace_handle_v1 as ext_ws,
    ext_workspace_manager_v1 as ext_ws_manager,
};

use crate::protocols::toplevel_info_v1::client::{
    zcosmic_toplevel_handle_v1 as cosmic_top, zcosmic_toplevel_info_v1 as cosmic_info,
};
use crate::protocols::toplevel_management_v1::client::zcosmic_toplevel_manager_v1 as cosmic_manager;

/// A window, as the snapshot saw it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Window {
    pub app_id: String,
    pub title: String,
    pub activated: bool,
    /// Indexes into [`Snapshot::workspaces`].
    pub workspaces: Vec<usize>,
}

/// A workspace, as the snapshot saw it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Workspace {
    pub name: String,
    /// Position in its group's grid (COSMIC: a single row).
    pub coordinates: Vec<u32>,
    pub active: bool,
    /// Index into the groups, in announcement order (one per output).
    pub group: Option<usize>,
}

/// Everything a command decides on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub windows: Vec<Window>,
    pub workspaces: Vec<Workspace>,
}

impl Snapshot {
    pub fn focused(&self) -> Option<usize> {
        self.windows.iter().position(|w| w.activated)
    }
}

/// Whether a window's Wayland `app_id` belongs to the `.desktop` id an
/// intent names. They often differ in form (`org.mozilla.firefox` vs
/// `firefox`, `com.system76.CosmicTerm` vs itself), so compare whole ids and
/// their last dotted parts, ignoring case.
pub fn app_id_matches(desktop_id: &str, app_id: &str) -> bool {
    let last = |s: &str| s.rsplit('.').next().unwrap_or(s).to_ascii_lowercase();
    let (d, a) = (desktop_id.to_ascii_lowercase(), app_id.to_ascii_lowercase());
    d == a || last(&d) == last(&a) || last(&d) == a || d == last(&a)
}

/// The window to focus for an app known by any of `ids` (its `.desktop`
/// id, its `StartupWMClass`): one not already focused if there is one
/// (repeating "focus Firefox" focuses *a* Firefox), else the focused one.
pub fn find_app(snap: &Snapshot, ids: &[String]) -> Option<usize> {
    let matching: Vec<usize> = (0..snap.windows.len())
        .filter(|&i| {
            ids.iter()
                .any(|id| app_id_matches(id, &snap.windows[i].app_id))
        })
        .collect();
    matching
        .iter()
        .copied()
        .find(|&i| !snap.windows[i].activated)
        .or_else(|| matching.first().copied())
}

/// Workspace number `n` as the user counts them: in the group (output) of
/// the focused window's workspace, or of the active one, the workspace
/// named `n`, else the `n`th by coordinates.
pub fn find_workspace(snap: &Snapshot, n: u32) -> Option<usize> {
    let from_focus = snap
        .focused()
        .and_then(|w| snap.windows[w].workspaces.first().copied());
    let anchor = from_focus.or_else(|| snap.workspaces.iter().position(|w| w.active))?;
    let group = snap.workspaces[anchor].group;
    let mut in_group: Vec<usize> = (0..snap.workspaces.len())
        .filter(|&i| snap.workspaces[i].group == group)
        .collect();
    let name = n.to_string();
    if let Some(&i) = in_group.iter().find(|&&i| snap.workspaces[i].name == name) {
        return Some(i);
    }
    in_group.sort_by(|&a, &b| {
        snap.workspaces[a]
            .coordinates
            .cmp(&snap.workspaces[b].coordinates)
    });
    in_group.get((n as usize).checked_sub(1)?).copied()
}

/// The workspace "a new workspace" means: in the same group as
/// [`find_workspace`] anchors on, the first (by position) holding no
/// windows, else the last. COSMIC keeps a spare empty workspace at the end,
/// so the fallback only matters with dynamic workspaces off.
pub fn find_new_workspace(snap: &Snapshot) -> Option<usize> {
    let from_focus = snap
        .focused()
        .and_then(|w| snap.windows[w].workspaces.first().copied());
    let anchor = from_focus.or_else(|| snap.workspaces.iter().position(|w| w.active))?;
    let group = snap.workspaces[anchor].group;
    let mut in_group: Vec<usize> = (0..snap.workspaces.len())
        .filter(|&i| snap.workspaces[i].group == group)
        .collect();
    in_group.sort_by(|&a, &b| {
        snap.workspaces[a]
            .coordinates
            .cmp(&snap.workspaces[b].coordinates)
    });
    in_group
        .iter()
        .copied()
        .find(|&i| !snap.windows.iter().any(|w| w.workspaces.contains(&i)))
        .or_else(|| in_group.last().copied())
}

// ---- the Wayland side ------------------------------------------------------

struct Top {
    ext: ext_top::ExtForeignToplevelHandleV1,
    cosmic: Option<cosmic_top::ZcosmicToplevelHandleV1>,
    app_id: String,
    title: String,
    activated: bool,
    /// Its COSMIC `state` event has arrived.
    has_state: bool,
    workspaces: Vec<ObjectId>,
    outputs: Vec<wl_output::WlOutput>,
}

struct Ws {
    handle: ext_ws::ExtWorkspaceHandleV1,
    name: String,
    coordinates: Vec<u32>,
    active: bool,
}

#[derive(Default)]
struct State {
    /// Set once bound: every window announced gets its COSMIC handle at once.
    info: Option<cosmic_info::ZcosmicToplevelInfoV1>,
    /// `zcosmic_toplevel_info_v1.done` seen: the window states are in.
    info_done: bool,
    tops: Vec<Top>,
    workspaces: Vec<Ws>,
    /// (group handle, its workspaces' ids, its outputs)
    groups: Vec<(ObjectId, Vec<ObjectId>, Vec<wl_output::WlOutput>)>,
}

impl State {
    fn top(&mut self, id: &ObjectId) -> Option<&mut Top> {
        self.tops
            .iter_mut()
            .find(|t| &t.ext.id() == id || t.cosmic.as_ref().is_some_and(|c| &c.id() == id))
    }

    fn snapshot(&self) -> Snapshot {
        let ws_index = |id: &ObjectId| self.workspaces.iter().position(|w| &w.handle.id() == id);
        let group_of = |id: &ObjectId| self.groups.iter().position(|g| g.1.contains(id));
        Snapshot {
            windows: self
                .tops
                .iter()
                .map(|t| Window {
                    app_id: t.app_id.clone(),
                    title: t.title.clone(),
                    activated: t.activated,
                    workspaces: t.workspaces.iter().filter_map(ws_index).collect(),
                })
                .collect(),
            workspaces: self
                .workspaces
                .iter()
                .map(|w| Workspace {
                    name: w.name.clone(),
                    coordinates: w.coordinates.clone(),
                    active: w.active,
                    group: group_of(&w.handle.id()),
                })
                .collect(),
        }
    }
}

/// The connection and its state, owned by the service thread.
struct WindowControl {
    conn: Connection,
    queue: wayland_client::EventQueue<State>,
    state: State,
    seat: wl_seat::WlSeat,
    manager: cosmic_manager::ZcosmicToplevelManagerV1,
    ws_manager: Option<ext_ws_manager::ExtWorkspaceManagerV1>,
}

impl WindowControl {
    /// Connect, bind, and wait until the initial window states are in.
    fn connect() -> anyhow::Result<Self> {
        let conn = Connection::connect_to_env().context("no Wayland session")?;
        let (globals, mut queue) = registry_queue_init::<State>(&conn)?;
        let qh = queue.handle();
        let list: ext_list::ExtForeignToplevelListV1 = globals
            .bind(&qh, 1..=1, ())
            .context("compositor lacks ext_foreign_toplevel_list_v1")?;
        let info: cosmic_info::ZcosmicToplevelInfoV1 = globals
            .bind(&qh, 2..=3, ())
            .context("compositor lacks zcosmic_toplevel_info_v1 v2+")?;
        let manager: cosmic_manager::ZcosmicToplevelManagerV1 = globals
            .bind(&qh, 1..=4, ())
            .context("compositor lacks zcosmic_toplevel_manager_v1")?;
        let seat: wl_seat::WlSeat = globals.bind(&qh, 1..=1, ()).context("no wl_seat")?;
        let ws_manager: Option<ext_ws_manager::ExtWorkspaceManagerV1> =
            globals.bind(&qh, 1..=1, ()).ok();
        // Outputs must be bound for output_enter to name them.
        for g in globals.contents().clone_list() {
            if g.interface == wl_output::WlOutput::interface().name {
                let _: wl_output::WlOutput =
                    globals.registry().bind(g.name, g.version.min(4), &qh, ());
            }
        }
        let _ = list;
        let mut state = State {
            info: Some(info),
            ..State::default()
        };
        queue.roundtrip(&mut state)?;
        // The protocol says the states follow "immediately", but cosmic-comp
        // sends them on its next refresh, after a plain round trip has
        // returned, and sends no `done` for them (both seen live). So: wait
        // until every window's `state` is in (or `done`, should a compositor
        // send it), briefly, and go on without the rest rather than hang.
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(250);
        let complete = |s: &State| s.info_done || s.tops.iter().all(|t| t.has_state);
        queue.roundtrip(&mut state)?;
        while !complete(&state) && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(1));
            queue.roundtrip(&mut state)?;
        }
        if !complete(&state) {
            tracing::debug!("window states incomplete after 250 ms");
        }
        Ok(Self {
            conn,
            queue,
            state,
            seat,
            manager,
            ws_manager,
        })
    }

    fn snapshot(&self) -> Snapshot {
        self.state.snapshot()
    }

    fn finish(&mut self) -> anyhow::Result<()> {
        self.conn.flush()?;
        self.queue.roundtrip(&mut self.state)?;
        Ok(())
    }

    fn cosmic(&self, i: usize) -> anyhow::Result<&cosmic_top::ZcosmicToplevelHandleV1> {
        self.state.tops[i]
            .cosmic
            .as_ref()
            .context("window has no COSMIC handle")
    }

    fn focused(&self) -> anyhow::Result<usize> {
        self.snapshot().focused().context("no window is focused")
    }

    /// Focus a window of the app known by any of `ids`. Returns its title.
    fn focus_app(&mut self, ids: &[String]) -> anyhow::Result<String> {
        let Some(i) = find_app(&self.snapshot(), ids) else {
            bail!("no open window of `{}`", ids.join("` / `"));
        };
        self.manager.activate(self.cosmic(i)?, &self.seat);
        let title = self.state.tops[i].title.clone();
        self.finish()?;
        Ok(title)
    }

    fn maximize_focused(&mut self) -> anyhow::Result<()> {
        let i = self.focused()?;
        self.manager.set_maximized(self.cosmic(i)?);
        self.finish()
    }

    fn minimize_focused(&mut self) -> anyhow::Result<()> {
        let i = self.focused()?;
        self.manager.set_minimized(self.cosmic(i)?);
        self.finish()
    }

    /// Move the focused window to workspace `n` (as [`find_workspace`]
    /// counts), on the same output.
    fn move_focused_to_workspace(&mut self, n: u32) -> anyhow::Result<()> {
        if self.manager.version() < 4 {
            bail!("the compositor's toplevel manager is older than v4");
        }
        let snap = self.snapshot();
        let i = self.focused()?;
        let ws = find_workspace(&snap, n).with_context(|| format!("no workspace {n}"))?;
        let output = self
            .output_for(ws, i)
            .context("no output for that workspace")?;
        let handle = self.state.workspaces[ws].handle.clone();
        self.manager
            .move_to_ext_workspace(self.cosmic(i)?, &handle, &output);
        self.finish()
    }

    /// Show workspace `n`.
    fn switch_workspace(&mut self, n: u32) -> anyhow::Result<()> {
        let manager = self
            .ws_manager
            .clone()
            .context("compositor lacks ext_workspace_manager_v1")?;
        let ws =
            find_workspace(&self.snapshot(), n).with_context(|| format!("no workspace {n}"))?;
        self.state.workspaces[ws].handle.activate();
        manager.commit();
        self.finish()
    }

    /// Show an empty workspace (see [`find_new_workspace`]).
    fn switch_new_workspace(&mut self) -> anyhow::Result<()> {
        let manager = self
            .ws_manager
            .clone()
            .context("compositor lacks ext_workspace_manager_v1")?;
        let ws = find_new_workspace(&self.snapshot()).context("no workspace to switch to")?;
        self.state.workspaces[ws].handle.activate();
        manager.commit();
        self.finish()
    }

    /// The output workspace `ws` is on: its group's, else the window's.
    fn output_for(&self, ws: usize, window: usize) -> Option<wl_output::WlOutput> {
        let id = self.state.workspaces[ws].handle.id();
        self.state
            .groups
            .iter()
            .find(|g| g.1.contains(&id))
            .and_then(|g| g.2.first().cloned())
            .or_else(|| self.state.tops[window].outputs.first().cloned())
    }
}

type Job = Box<dyn FnOnce(&mut WindowControl) + Send>;

impl WindowControl {
    /// Dispatch events as they arrive and run jobs as they're sent, until
    /// every sender is gone. `wake` is written to after each job is queued,
    /// so a job never waits for the compositor to say something first.
    fn serve(mut self, jobs: mpsc::Receiver<Job>, mut wake: UnixStream) {
        use rustix::event::{PollFd, PollFlags};
        loop {
            if let Err(e) = self.queue.dispatch_pending(&mut self.state) {
                tracing::warn!(error = %e, "window service: dispatch failed");
                return;
            }
            let _ = self.conn.flush();
            loop {
                match jobs.try_recv() {
                    Ok(job) => job(&mut self),
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => return,
                }
            }
            let Some(guard) = self.queue.prepare_read() else {
                continue;
            };
            let (wl_ready, wake_ready) = {
                let wl_fd = guard.connection_fd();
                let mut fds = [
                    PollFd::new(&wl_fd, PollFlags::IN),
                    PollFd::new(&wake, PollFlags::IN),
                ];
                if rustix::event::poll(&mut fds, None).is_err() {
                    continue;
                }
                (!fds[0].revents().is_empty(), !fds[1].revents().is_empty())
            };
            if wl_ready {
                if let Err(e) = guard.read() {
                    tracing::warn!(error = %e, "window service: connection lost");
                    return;
                }
            } else {
                drop(guard);
            }
            if wake_ready {
                let mut buf = [0u8; 64];
                let _ = wake.read(&mut buf);
            }
        }
    }
}

/// Window verbs over a persistent connection (see the module docs). Cheap
/// to share behind an `Arc`; dropping it stops the thread.
pub struct WindowService {
    jobs: Option<mpsc::Sender<Job>>,
    wake: UnixStream,
    thread: Option<std::thread::JoinHandle<()>>,
}

/// How long a caller waits for the service thread.
const CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(1);

impl WindowService {
    /// Connect and start the service thread. Fails, naming what's missing,
    /// on a compositor without the protocols (GNOME, for now).
    pub fn start() -> anyhow::Result<Self> {
        let (jobs, rx) = mpsc::channel::<Job>();
        let (wake, wake_rx) = UnixStream::pair()?;
        wake_rx.set_nonblocking(true)?;
        let (ready_tx, ready_rx) = mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("cosmo-windows".into())
            .spawn(move || match WindowControl::connect() {
                Ok(control) => {
                    let _ = ready_tx.send(Ok(()));
                    control.serve(rx, wake_rx);
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                }
            })?;
        ready_rx
            .recv()
            .map_err(|_| anyhow::anyhow!("window service died while connecting"))??;
        Ok(Self {
            jobs: Some(jobs),
            wake,
            thread: Some(thread),
        })
    }

    fn call<R: Send + 'static>(
        &self,
        f: impl FnOnce(&mut WindowControl) -> R + Send + 'static,
    ) -> anyhow::Result<R> {
        let (tx, rx) = mpsc::channel();
        self.jobs
            .as_ref()
            .context("window service stopped")?
            .send(Box::new(move |c| {
                let _ = tx.send(f(c));
            }))
            .map_err(|_| anyhow::anyhow!("window service stopped"))?;
        let _ = (&self.wake).write(&[1]);
        rx.recv_timeout(CALL_TIMEOUT)
            .map_err(|_| anyhow::anyhow!("window service did not answer"))
    }

    pub fn snapshot(&self) -> anyhow::Result<Snapshot> {
        self.call(|c| c.snapshot())
    }

    /// Focus a window of the app known by any of `ids` (its `.desktop` id,
    /// its `StartupWMClass`). Returns the window's title.
    pub fn focus_app(&self, ids: &[String]) -> anyhow::Result<String> {
        let ids = ids.to_vec();
        self.call(move |c| c.focus_app(&ids))?
    }

    pub fn maximize_focused(&self) -> anyhow::Result<()> {
        self.call(|c| c.maximize_focused())?
    }

    pub fn minimize_focused(&self) -> anyhow::Result<()> {
        self.call(|c| c.minimize_focused())?
    }

    /// Move the focused window to workspace `n` (as [`find_workspace`]
    /// counts), on the same output.
    pub fn move_focused_to_workspace(&self, n: u32) -> anyhow::Result<()> {
        self.call(move |c| c.move_focused_to_workspace(n))?
    }

    /// Show workspace `n`.
    pub fn switch_workspace(&self, n: u32) -> anyhow::Result<()> {
        self.call(move |c| c.switch_workspace(n))?
    }

    /// Show an empty workspace, so what opens next lands alone.
    pub fn switch_new_workspace(&self) -> anyhow::Result<()> {
        self.call(|c| c.switch_new_workspace())?
    }
}

impl Drop for WindowService {
    fn drop(&mut self) {
        self.jobs = None;
        let _ = (&self.wake).write(&[1]);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

// ---- event plumbing ----------------------------------------------------------

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

macro_rules! ignore {
    ($($t:ty),*) => {$(
        impl Dispatch<$t, ()> for State {
            fn event(_: &mut Self, _: &$t, _: <$t as Proxy>::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
        }
    )*};
}
ignore!(
    wl_seat::WlSeat,
    wl_output::WlOutput,
    cosmic_manager::ZcosmicToplevelManagerV1
);

impl Dispatch<ext_list::ExtForeignToplevelListV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ext_list::ExtForeignToplevelListV1,
        event: ext_list::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let ext_list::Event::Toplevel { toplevel } = event {
            let cosmic = state
                .info
                .as_ref()
                .map(|info| info.get_cosmic_toplevel(&toplevel, qh, ()));
            state.tops.push(Top {
                ext: toplevel,
                cosmic,
                app_id: String::new(),
                title: String::new(),
                activated: false,
                has_state: false,
                workspaces: Vec::new(),
                outputs: Vec::new(),
            });
        }
    }

    event_created_child!(State, ext_list::ExtForeignToplevelListV1, [
        ext_list::EVT_TOPLEVEL_OPCODE => (ext_top::ExtForeignToplevelHandleV1, ()),
    ]);
}

impl Dispatch<ext_top::ExtForeignToplevelHandleV1, ()> for State {
    fn event(
        state: &mut Self,
        handle: &ext_top::ExtForeignToplevelHandleV1,
        event: ext_top::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let id = handle.id();
        match event {
            ext_top::Event::AppId { app_id } => {
                if let Some(t) = state.top(&id) {
                    t.app_id = app_id;
                }
            }
            ext_top::Event::Title { title } => {
                if let Some(t) = state.top(&id) {
                    t.title = title;
                }
            }
            ext_top::Event::Closed => state.tops.retain(|t| {
                let keep = t.ext.id() != id;
                if !keep && let Some(c) = &t.cosmic {
                    c.destroy();
                }
                keep
            }),
            _ => {}
        }
    }
}

impl Dispatch<cosmic_info::ZcosmicToplevelInfoV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &cosmic_info::ZcosmicToplevelInfoV1,
        event: cosmic_info::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let cosmic_info::Event::Done = event {
            state.info_done = true;
        }
    }

    // v1's `toplevel` event (deprecated since v2, not sent at v3) creates
    // handles; declared so an older compositor can't crash the client.
    event_created_child!(State, cosmic_info::ZcosmicToplevelInfoV1, [
        cosmic_info::EVT_TOPLEVEL_OPCODE => (cosmic_top::ZcosmicToplevelHandleV1, ()),
    ]);
}

/// `zcosmic_toplevel_handle_v1` state value for "activated".
const ACTIVATED: u32 = 2;

impl Dispatch<cosmic_top::ZcosmicToplevelHandleV1, ()> for State {
    fn event(
        state: &mut Self,
        handle: &cosmic_top::ZcosmicToplevelHandleV1,
        event: cosmic_top::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let id = handle.id();
        let Some(t) = state.top(&id) else { return };
        match event {
            cosmic_top::Event::State { state } => {
                t.has_state = true;
                t.activated = state
                    .chunks_exact(4)
                    .any(|c| u32::from_ne_bytes([c[0], c[1], c[2], c[3]]) == ACTIVATED);
            }
            cosmic_top::Event::ExtWorkspaceEnter { workspace } => {
                t.workspaces.push(workspace.id());
            }
            cosmic_top::Event::ExtWorkspaceLeave { workspace } => {
                t.workspaces.retain(|w| *w != workspace.id());
            }
            cosmic_top::Event::OutputEnter { output } => t.outputs.push(output),
            cosmic_top::Event::OutputLeave { output } => t.outputs.retain(|o| *o != output),
            _ => {}
        }
    }
}

impl Dispatch<ext_ws_manager::ExtWorkspaceManagerV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ext_ws_manager::ExtWorkspaceManagerV1,
        event: ext_ws_manager::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_ws_manager::Event::WorkspaceGroup { workspace_group } => {
                state
                    .groups
                    .push((workspace_group.id(), Vec::new(), Vec::new()));
            }
            ext_ws_manager::Event::Workspace { workspace } => state.workspaces.push(Ws {
                handle: workspace,
                name: String::new(),
                coordinates: Vec::new(),
                active: false,
            }),
            _ => {}
        }
    }

    event_created_child!(State, ext_ws_manager::ExtWorkspaceManagerV1, [
        ext_ws_manager::EVT_WORKSPACE_GROUP_OPCODE => (ext_group::ExtWorkspaceGroupHandleV1, ()),
        ext_ws_manager::EVT_WORKSPACE_OPCODE => (ext_ws::ExtWorkspaceHandleV1, ()),
    ]);
}

impl Dispatch<ext_group::ExtWorkspaceGroupHandleV1, ()> for State {
    fn event(
        state: &mut Self,
        handle: &ext_group::ExtWorkspaceGroupHandleV1,
        event: ext_group::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let id = handle.id();
        let Some(g) = state.groups.iter_mut().find(|g| g.0 == id) else {
            return;
        };
        match event {
            ext_group::Event::WorkspaceEnter { workspace } => g.1.push(workspace.id()),
            ext_group::Event::WorkspaceLeave { workspace } => g.1.retain(|w| *w != workspace.id()),
            ext_group::Event::OutputEnter { output } => g.2.push(output),
            ext_group::Event::OutputLeave { output } => g.2.retain(|o| *o != output),
            ext_group::Event::Removed => {
                handle.destroy();
                state.groups.retain(|g| g.0 != id);
            }
            _ => {}
        }
    }
}

impl Dispatch<ext_ws::ExtWorkspaceHandleV1, ()> for State {
    fn event(
        state: &mut Self,
        handle: &ext_ws::ExtWorkspaceHandleV1,
        event: ext_ws::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let id = handle.id();
        let Some(w) = state.workspaces.iter_mut().find(|w| w.handle.id() == id) else {
            return;
        };
        match event {
            ext_ws::Event::Name { name } => w.name = name,
            ext_ws::Event::Coordinates { coordinates } => {
                w.coordinates = coordinates
                    .chunks_exact(4)
                    .map(|c| u32::from_ne_bytes([c[0], c[1], c[2], c[3]]))
                    .collect();
            }
            ext_ws::Event::State { state } => {
                w.active = state
                    .into_result()
                    .is_ok_and(|s| s.contains(ext_ws::State::Active));
            }
            ext_ws::Event::Removed => {
                handle.destroy();
                state.workspaces.retain(|w| w.handle.id() != id);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_ids_match_across_forms() {
        assert!(app_id_matches("firefox", "firefox"));
        assert!(app_id_matches("org.mozilla.firefox", "firefox"));
        assert!(app_id_matches("firefox", "org.mozilla.firefox"));
        assert!(app_id_matches(
            "com.system76.CosmicTerm",
            "com.system76.CosmicTerm"
        ));
        // Not by id alone: that's what StartupWMClass is for (find_app).
        assert!(!app_id_matches("com.spotify.Client", "spotify"));
        assert!(!app_id_matches("firefox", "thunderbird"));
    }

    fn snap() -> Snapshot {
        let ws = |name: &str, x: u32, active: bool, group: usize| Workspace {
            name: name.into(),
            coordinates: vec![x],
            active,
            group: Some(group),
        };
        Snapshot {
            windows: vec![
                Window {
                    app_id: "firefox".into(),
                    activated: true,
                    workspaces: vec![0],
                    ..Default::default()
                },
                Window {
                    app_id: "codium".into(),
                    workspaces: vec![1],
                    ..Default::default()
                },
                Window {
                    app_id: "firefox".into(),
                    workspaces: vec![1],
                    ..Default::default()
                },
            ],
            workspaces: vec![
                ws("1", 0, true, 0),
                ws("2", 1, false, 0),
                ws("3", 2, false, 0),
                // A second output's workspaces, also named from 1.
                ws("1", 0, true, 1),
                ws("2", 1, false, 1),
            ],
        }
    }

    fn ids(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_new_workspace_is_the_first_empty_one() {
        // snap(): windows on workspaces 1 and 2, so 3 is the spare.
        assert_eq!(find_new_workspace(&snap()), Some(2));
        let mut full = snap();
        full.windows[0].workspaces = vec![2];
        // Now 1 is empty, and it's the first.
        assert_eq!(find_new_workspace(&full), Some(0));
    }

    #[test]
    fn focusing_an_app_prefers_its_unfocused_window() {
        assert_eq!(find_app(&snap(), &ids(&["firefox"])), Some(2));
        assert_eq!(find_app(&snap(), &ids(&["codium"])), Some(1));
        assert_eq!(find_app(&snap(), &ids(&["spotify"])), None);
    }

    /// Spotify's entry is `com.spotify.Client`, its window `spotify`: the
    /// entry's StartupWMClass links them.
    #[test]
    fn the_wm_class_finds_windows_the_id_cant() {
        let mut s = snap();
        s.windows[1].app_id = "spotify".into();
        assert_eq!(find_app(&s, &ids(&["com.spotify.Client"])), None);
        assert_eq!(
            find_app(&s, &ids(&["com.spotify.Client", "spotify"])),
            Some(1)
        );
    }

    #[test]
    fn workspace_n_is_counted_on_the_focused_windows_output() {
        assert_eq!(find_workspace(&snap(), 3), Some(2));
        assert_eq!(
            find_workspace(&snap(), 2),
            Some(1),
            "not the other output's 2"
        );
        assert_eq!(find_workspace(&snap(), 9), None);
        assert_eq!(find_workspace(&snap(), 0), None);
    }

    #[test]
    fn unnamed_workspaces_are_counted_by_position() {
        let mut s = snap();
        for w in &mut s.workspaces {
            w.name.clear();
        }
        s.workspaces.swap(1, 2); // announcement order isn't position
        assert_eq!(
            s.workspaces[find_workspace(&s, 2).unwrap()].coordinates,
            [1]
        );
    }
}
