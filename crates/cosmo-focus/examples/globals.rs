//! List the Wayland globals the compositor advertises, with versions: what
//! the window verbs can bind (phase-4 spec §4.4).
//!
//! `cargo run -p cosmo-focus --example globals [filter]`

use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_registry;
use wayland_client::{Connection, Dispatch, QueueHandle};

struct State;

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

fn main() -> anyhow::Result<()> {
    let filter = std::env::args().nth(1).unwrap_or_default();
    let conn = Connection::connect_to_env()?;
    let (globals, _queue) = registry_queue_init::<State>(&conn)?;
    let mut list = globals.contents().clone_list();
    list.sort_by(|a, b| a.interface.cmp(&b.interface));
    for g in list.iter().filter(|g| g.interface.contains(&filter)) {
        println!("{:<48} v{}", g.interface, g.version);
    }
    Ok(())
}
