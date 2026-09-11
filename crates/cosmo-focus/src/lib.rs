//! Focus + workspace tracking from the compositor (plan §1.3, phase-0
//! findings §3.1): `zcosmic_toplevel_info_v1` on a persistent Wayland
//! connection, so the mirror stays live and listing is free.
//!
//! Why not the agent's `focused_window`: it is silently broken on COSMIC —
//! `null` focus, `focused: false` for every window, `workspace: null`
//! everywhere (findings §3 item 1). Invariant #9: compositor is the source
//! of truth, the agent's answer is advisory only.
//!
//! GNOME degradation: cosmic-protocols is absent there, so
//! [`FocusMirror::connect`] errors and the daemon runs with no mirror; the
//! phase-1 portability claim holds because nothing in phase 1 requires
//! focus info.
//!
//! **The input-method invariant does not live here** — this connection binds
//! only `zcosmic_toplevel_info_v1` and `zcosmic_workspace_manager_v1`. See
//! `cosmo_type::Keyboard::connect` for the `zwp_input_method_v2` tripwire.

use std::sync::Mutex;

use anyhow::Context as _;
use cosmic_protocols::toplevel_info::v1::client::{
    zcosmic_toplevel_handle_v1, zcosmic_toplevel_info_v1,
};
use cosmic_protocols::workspace::v1::client::{
    zcosmic_workspace_group_handle_v1, zcosmic_workspace_handle_v1, zcosmic_workspace_manager_v1,
};
use wayland_client::globals::GlobalListContents;
use wayland_client::protocol::{wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, event_created_child};

/// `zcosmic_toplevel_handle_v1` state bit for "activated" (value 2).
const ACTIVATED: u32 = 2;

/// One mirrored toplevel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToplevelInfo {
    pub app_id: Option<String>,
    pub title: Option<String>,
    /// The compositor's truth about focus (invariant #9).
    pub activated: bool,
    /// Workspace names this toplevel lives on.
    pub workspaces: Vec<String>,
}

/// Snapshot of everything the mirror knows.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FocusState {
    pub toplevels: Vec<ToplevelInfo>,
}

impl FocusState {
    /// The focused window (normally exactly one is `activated`).
    pub fn focused(&self) -> Option<&ToplevelInfo> {
        self.toplevels.iter().find(|t| t.activated)
    }
}

/// A one-shot mirror snapshot. Phase 1 connects, pumps two roundtrips, and
/// stores the result; a persistent dispatch thread arrives with the daemon's
/// hotword loop (§3.3) — the API already reads from a stored snapshot.
pub struct FocusMirror {
    snapshot: Mutex<FocusState>,
}

impl FocusMirror {
    /// Connect, bind, roundtrip twice (registry burst → toplevels), and
    /// store the snapshot. Errors when the compositor lacks the protocols
    /// (GNOME): the daemon then runs with no mirror and `available()` false.
    pub fn connect() -> anyhow::Result<Self> {
        let state = pump()?;
        let focus = FocusState {
            toplevels: state
                .toplevels
                .iter()
                .map(|t| ToplevelInfo {
                    app_id: t.app_id.clone(),
                    title: t.title.clone(),
                    activated: t.state.contains(&ACTIVATED),
                    workspaces: t
                        .workspaces
                        .iter()
                        .filter_map(|w| resolve_workspace(&state, w))
                        .collect(),
                })
                .collect(),
        };
        Ok(Self {
            snapshot: Mutex::new(focus),
        })
    }

    /// Whether the compositor protocols were available at connect time.
    pub fn available(&self) -> bool {
        true
    }

    /// Snapshot of the current mirror.
    pub fn snapshot(&self) -> FocusState {
        self.snapshot.lock().unwrap().clone()
    }

    /// Focused `app_id`, for hotword biasing (plan §3.3).
    pub fn focused_app_id(&self) -> Option<String> {
        self.snapshot().focused().and_then(|t| t.app_id.clone())
    }
}

fn resolve_workspace(
    state: &State,
    handle: &zcosmic_workspace_handle_v1::ZcosmicWorkspaceHandleV1,
) -> Option<String> {
    state
        .workspaces
        .iter()
        .flat_map(|(_, s)| s.iter())
        .find(|(w, _)| w == handle)
        .and_then(|(_, n)| n.clone())
}

/// Connect and pump the registry + protocols to a filled [`State`].
fn pump() -> anyhow::Result<State> {
    let conn = Connection::connect_to_env().context("connecting to the compositor")?;
    let mut queue = conn.new_event_queue();
    let qh = queue.handle();
    let _display = conn.display().get_registry(&qh, ());
    let mut state = State::default();
    queue.roundtrip(&mut state)?;
    queue.roundtrip(&mut state)?;
    // Third: handles created during roundtrip two deliver their state.
    queue.roundtrip(&mut state)?;
    if state.toplevel_info.is_none() {
        anyhow::bail!("compositor lacks zcosmic_toplevel_info_v1 — focus mirror unavailable");
    }
    Ok(state)
}

/// Workspace groups with their named handles.
type WorkspaceGroup = (
    zcosmic_workspace_group_handle_v1::ZcosmicWorkspaceGroupHandleV1,
    Vec<(
        zcosmic_workspace_handle_v1::ZcosmicWorkspaceHandleV1,
        Option<String>,
    )>,
);

#[derive(Default)]
struct State {
    toplevel_info: Option<zcosmic_toplevel_info_v1::ZcosmicToplevelInfoV1>,
    workspace_manager: Option<zcosmic_workspace_manager_v1::ZcosmicWorkspaceManagerV1>,
    toplevels: Vec<Toplevel>,
    workspaces: Vec<WorkspaceGroup>,
}

struct Toplevel {
    handle: zcosmic_toplevel_handle_v1::ZcosmicToplevelHandleV1,
    title: Option<String>,
    app_id: Option<String>,
    workspaces: Vec<zcosmic_workspace_handle_v1::ZcosmicWorkspaceHandleV1>,
    state: Vec<u32>,
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version: _,
        } = event
        {
            match &*interface {
                "zcosmic_toplevel_info_v1" => {
                    // Bind v1: the deprecated `toplevel` event fires at v1
                    // (the flow the cosmic-protocols example documents).
                    // v2+ requires pairing with ext_foreign_toplevel_list
                    // and explicit get_cosmic_toplevel per handle — revisit
                    // if v1 support disappears upstream.
                    state.toplevel_info = Some(
                        registry.bind::<zcosmic_toplevel_info_v1::ZcosmicToplevelInfoV1, _, _>(
                            name,
                            1,
                            qh,
                            (),
                        ),
                    );
                }
                "zcosmic_workspace_manager_v1" => {
                    state.workspace_manager = Some(
                        registry
                            .bind::<zcosmic_workspace_manager_v1::ZcosmicWorkspaceManagerV1, _, _>(
                                name,
                                2,
                                qh,
                                (),
                            ),
                    );
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        // Same handling: dispatch data type does not change semantics here.
        Self::event_impl(state, registry, event, qh)
    }
}

impl State {
    fn event_impl(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_registry::Event::Global {
            name,
            interface,
            version: _,
        } = event
        {
            match &*interface {
                "zcosmic_toplevel_info_v1" => {
                    state.toplevel_info = Some(registry.bind(name, 1, qh, ()));
                }
                "zcosmic_workspace_manager_v1" => {
                    state.workspace_manager = Some(registry.bind(name, 2, qh, ()));
                }
                _ => {}
            }
        }
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for State {
    fn event(
        _: &mut Self,
        _: &wl_seat::WlSeat,
        _: wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<zcosmic_toplevel_info_v1::ZcosmicToplevelInfoV1, ()> for State {
    fn event(
        state: &mut Self,
        _info: &zcosmic_toplevel_info_v1::ZcosmicToplevelInfoV1,
        event: zcosmic_toplevel_info_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zcosmic_toplevel_info_v1::Event::Toplevel { toplevel } = event {
            state.toplevels.push(Toplevel {
                handle: toplevel,
                title: None,
                app_id: None,
                workspaces: Vec::new(),
                state: Vec::new(),
            });
        }
    }

    event_created_child!(
        State,
        zcosmic_toplevel_info_v1::ZcosmicToplevelInfoV1,
        [
            zcosmic_toplevel_info_v1::EVT_TOPLEVEL_OPCODE => (zcosmic_toplevel_handle_v1::ZcosmicToplevelHandleV1, ()),
        ]
    );
}

impl Dispatch<zcosmic_toplevel_handle_v1::ZcosmicToplevelHandleV1, ()> for State {
    fn event(
        state: &mut Self,
        toplevel: &zcosmic_toplevel_handle_v1::ZcosmicToplevelHandleV1,
        event: zcosmic_toplevel_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(info) = state.toplevels.iter_mut().find(|t| &t.handle == toplevel) else {
            return;
        };
        match event {
            zcosmic_toplevel_handle_v1::Event::Title { title } => info.title = Some(title),
            zcosmic_toplevel_handle_v1::Event::AppId { app_id } => info.app_id = Some(app_id),
            zcosmic_toplevel_handle_v1::Event::WorkspaceEnter { workspace } => {
                info.workspaces.push(workspace);
            }
            zcosmic_toplevel_handle_v1::Event::WorkspaceLeave { workspace } => {
                info.workspaces.retain(|w| w != &workspace);
            }
            zcosmic_toplevel_handle_v1::Event::State { state: bits } => {
                info.state = bits
                    .chunks_exact(4)
                    .map(|c| u32::from_ne_bytes(c.try_into().unwrap()))
                    .collect();
            }
            _ => {}
        }
    }
}

impl Dispatch<zcosmic_workspace_manager_v1::ZcosmicWorkspaceManagerV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &zcosmic_workspace_manager_v1::ZcosmicWorkspaceManagerV1,
        event: zcosmic_workspace_manager_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zcosmic_workspace_manager_v1::Event::WorkspaceGroup { workspace_group } = event {
            state.workspaces.push((workspace_group, Vec::new()));
        }
    }

    event_created_child!(
        State,
        zcosmic_workspace_manager_v1::ZcosmicWorkspaceManagerV1,
        [
            zcosmic_workspace_manager_v1::EVT_WORKSPACE_GROUP_OPCODE => (zcosmic_workspace_group_handle_v1::ZcosmicWorkspaceGroupHandleV1, ()),
        ]
    );
}

impl Dispatch<zcosmic_workspace_group_handle_v1::ZcosmicWorkspaceGroupHandleV1, ()> for State {
    fn event(
        state: &mut Self,
        group: &zcosmic_workspace_group_handle_v1::ZcosmicWorkspaceGroupHandleV1,
        event: <zcosmic_workspace_group_handle_v1::ZcosmicWorkspaceGroupHandleV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zcosmic_workspace_group_handle_v1::Event::Workspace { workspace } = event
            && let Some((_, spaces)) = state.workspaces.iter_mut().find(|(g, _)| g == group)
        {
            spaces.push((workspace, None));
        }
    }

    event_created_child!(
        State,
        zcosmic_workspace_group_handle_v1::ZcosmicWorkspaceGroupHandleV1,
        [
            zcosmic_workspace_group_handle_v1::EVT_WORKSPACE_OPCODE => (zcosmic_workspace_handle_v1::ZcosmicWorkspaceHandleV1, ()),
        ]
    );
}

impl Dispatch<zcosmic_workspace_handle_v1::ZcosmicWorkspaceHandleV1, ()> for State {
    fn event(
        state: &mut Self,
        workspace: &zcosmic_workspace_handle_v1::ZcosmicWorkspaceHandleV1,
        event: <zcosmic_workspace_handle_v1::ZcosmicWorkspaceHandleV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let zcosmic_workspace_handle_v1::Event::Name { name } = event
            && let Some((_, n)) = state
                .workspaces
                .iter_mut()
                .flat_map(|(_, s)| s.iter_mut())
                .find(|(w, _)| w == workspace)
        {
            *n = Some(name);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activated_bit_is_two() {
        // xdg-toplevel state 2 == activated, matching the protocol enum.
        assert_eq!(ACTIVATED, 2);
    }

    #[test]
    fn focused_picks_activated_toplevel() {
        let state = FocusState {
            toplevels: vec![
                ToplevelInfo {
                    app_id: Some("editor".into()),
                    title: None,
                    activated: false,
                    workspaces: vec!["1".into()],
                },
                ToplevelInfo {
                    app_id: Some("terminal".into()),
                    title: None,
                    activated: true,
                    workspaces: vec!["1".into()],
                },
            ],
        };
        assert_eq!(
            state.focused().and_then(|t| t.app_id.clone()),
            Some("terminal".into())
        );
    }
}
