//! Where there's no layer shell (GNOME on Wayland), no overlay: the
//! moments that matter become desktop notifications instead (blueprint
//! §9). Which moments is decided here, purely.

use cosmo_ipc::Event;

use crate::view::plain_words;

/// The notification an event deserves, if any: (summary, body).
pub fn for_event(event: &Event) -> Option<(String, String)> {
    match event {
        Event::Held { token, action } => Some((
            "cosmo needs your confirmation".into(),
            format!(
                "{} — confirm with: cosmo confirm {token}",
                plain_words(action, "")
            ),
        )),
        Event::Reply { text } if !text.trim().is_empty() => Some(("cosmo".into(), text.clone())),
        _ => None,
    }
}

/// Whether the compositor offers layer shell (the overlay's surface).
pub fn layer_shell_available() -> bool {
    layer_shell() == Some(true)
}

/// Whether the compositor offers layer shell; `None` with no Wayland
/// display to ask (`cosmo doctor` run from outside the session).
pub fn layer_shell() -> Option<bool> {
    use wayland_client::globals::{GlobalListContents, registry_queue_init};
    use wayland_client::protocol::wl_registry;
    use wayland_client::{Connection, Dispatch, QueueHandle};

    struct Probe;
    impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for Probe {
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
    let conn = Connection::connect_to_env().ok()?;
    let (globals, _queue) = registry_queue_init::<Probe>(&conn).ok()?;
    Some(
        globals
            .contents()
            .clone_list()
            .iter()
            .any(|g| g.interface == "zwlr_layer_shell_v1"),
    )
}

/// Follow the daemon and turn its important moments into notifications,
/// until the process is stopped.
pub async fn run() {
    let mut rx = cosmo_ipc::client::subscribe();
    let Ok(conn) = zbus::Connection::session().await else {
        eprintln!("cosmo-overlay: no layer shell and no session bus: nothing to show on");
        return;
    };
    while let Some(update) = rx.recv().await {
        if let cosmo_ipc::client::Update::Event(event) = update
            && let Some((summary, body)) = for_event(&event)
        {
            let hints: std::collections::HashMap<&str, zbus::zvariant::Value<'_>> =
                std::collections::HashMap::new();
            let _ = conn
                .call_method(
                    Some("org.freedesktop.Notifications"),
                    "/org/freedesktop/Notifications",
                    Some("org.freedesktop.Notifications"),
                    "Notify",
                    &(
                        "cosmo",
                        0u32,
                        "audio-input-microphone",
                        summary,
                        body,
                        Vec::<&str>::new(),
                        hints,
                        -1i32,
                    ),
                )
                .await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn holds_and_replies_notify_and_nothing_else_does() {
        let held = for_event(&Event::Held {
            token: "38e1b3fb".into(),
            action: "run_in_terminal".into(),
        })
        .unwrap();
        assert_eq!(held.0, "cosmo needs your confirmation");
        assert_eq!(
            held.1,
            "Running a command — confirm with: cosmo confirm 38e1b3fb"
        );
        assert_eq!(
            for_event(&Event::Reply {
                text: "Done.".into()
            }),
            Some(("cosmo".into(), "Done.".into()))
        );
        assert_eq!(for_event(&Event::Reply { text: " ".into() }), None);
        assert_eq!(for_event(&Event::Level { rms: 0.1 }), None);
        assert_eq!(
            for_event(&Event::State {
                state: cosmo_ipc::State::Listening
            }),
            None
        );
    }
}
