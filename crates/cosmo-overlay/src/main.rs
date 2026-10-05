//! `cosmo-overlay`: cosmo's face (phase-6 spec §6.3). A libcosmic layer
//! surface at the bottom centre of the screen, shown only while there's
//! something to see, never taking keyboard focus.
//!
//! It owns nothing: it follows the daemon's events (`cosmo_ipc::client`),
//! and its only actions are the local confirm and cancel of a held action,
//! the same commands `cosmo confirm` / `cosmo cancel` send.
//!
//! Redraw discipline: the subscription applies each burst of events to the
//! view model itself and emits at most one message per burst, only when
//! something visible changed (`view::apply_burst`); iced then coalesces
//! redraws onto the compositor's frame callbacks.

use cosmic::iced::platform_specific::runtime::wayland::layer_surface::{
    IcedOutput, SctkLayerSurfaceSettings,
};
use cosmic::iced::platform_specific::shell::commands::layer_surface::{
    Anchor, KeyboardInteractivity, Layer,
};
use cosmic::iced::widget::{Column, Row};
use cosmic::iced::{Alignment, Length, Subscription, window};
use cosmic::widget::{self, button, container, text};
use cosmic::{Element, Task};
use cosmo_ipc::State;
use cosmo_overlay::view::{View, WAVE_POINTS, apply_burst};
use libcosmic as cosmic;

const WIDTH: u32 = 560;
const BOTTOM_MARGIN: i32 = 48;

fn main() -> cosmic::iced::Result {
    // For `cosmo doctor`: 0 = layer shell, 1 = none, 2 = no display.
    if std::env::args().nth(1).as_deref() == Some("--check-layer-shell") {
        std::process::exit(match cosmo_overlay::notify::layer_shell() {
            Some(true) => 0,
            Some(false) => 1,
            None => 2,
        });
    }
    // No layer shell (GNOME on Wayland): notifications instead of a face.
    if !cosmo_overlay::notify::layer_shell_available() {
        eprintln!("cosmo-overlay: no layer shell here; using notifications");
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime")
            .block_on(cosmo_overlay::notify::run());
        return Ok(());
    }
    let settings = cosmic::app::Settings::default()
        .no_main_window(true)
        .autosize(true)
        .transparent(true)
        .exit_on_close(false);
    cosmic::app::run::<Overlay>(settings, ())
}

struct Overlay {
    core: cosmic::app::Core,
    view: View,
    /// The layer surface, while the overlay is on screen.
    surface: Option<window::Id>,
    /// What the card showed when the user closed it: it stays closed
    /// until there's something new (another state, other held actions).
    dismissed: Option<(State, Vec<String>)>,
}

#[derive(Debug, Clone)]
enum Message {
    /// The view model after a burst that changed something visible.
    View(Box<View>),
    Confirm(String),
    Cancel(String),
    /// The card's ✕: close it, cancelling whatever it was waiting on.
    Dismiss,
    /// A confirm or cancel came back (errors are logged; the daemon's own
    /// events update the view).
    Sent(Result<(), String>),
}

impl cosmic::Application for Overlay {
    type Executor = cosmic::executor::Default;
    type Flags = ();
    type Message = Message;
    const APP_ID: &'static str = "io.github.speerdo.CosmoOverlay";

    fn core(&self) -> &cosmic::app::Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut cosmic::app::Core {
        &mut self.core
    }

    fn init(core: cosmic::app::Core, _: ()) -> (Self, Task<cosmic::Action<Message>>) {
        (
            Self {
                core,
                view: View::default(),
                surface: None,
                dismissed: None,
            },
            Task::none(),
        )
    }

    fn subscription(&self) -> Subscription<Message> {
        Subscription::run(events)
    }

    fn update(&mut self, message: Message) -> Task<cosmic::Action<Message>> {
        match message {
            Message::View(view) => {
                self.view = *view;
                if self.dismissed.as_ref() != Some(&dismiss_key(&self.view)) {
                    self.dismissed = None;
                }
                self.sync_surface()
            }
            Message::Dismiss => {
                self.dismissed = Some(dismiss_key(&self.view));
                // Closing the card is a "no" to what it was asking.
                let mut tasks: Vec<_> = self
                    .view
                    .holds
                    .iter()
                    .map(|h| {
                        send(cosmo_ipc::Command::Cancel {
                            token: h.token.clone(),
                        })
                    })
                    .collect();
                tasks.push(self.sync_surface());
                Task::batch(tasks)
            }
            Message::Confirm(token) => send(cosmo_ipc::Command::Confirm { token }),
            Message::Cancel(token) => send(cosmo_ipc::Command::Cancel { token }),
            Message::Sent(Err(e)) => {
                eprintln!("cosmo-overlay: {e}");
                Task::none()
            }
            Message::Sent(Ok(())) => Task::none(),
        }
    }

    fn view(&self) -> Element<'_, Message> {
        // No main window; everything is drawn in `view_window`.
        widget::Space::new().into()
    }

    fn view_window(&self, id: window::Id) -> Element<'_, Message> {
        if Some(id) != self.surface {
            return widget::Space::new().into();
        }
        // Grows and shrinks the surface to fit the card: a layer surface
        // doesn't size itself.
        cosmic::widget::autosize::autosize(card(&self.view), autosize_id()).into()
    }
}

impl Overlay {
    /// Create the surface when there's something to show, destroy it when
    /// there isn't.
    fn sync_surface(&mut self) -> Task<cosmic::Action<Message>> {
        let showing = self.view.visible() && self.dismissed.is_none();
        match (showing, self.surface) {
            (true, None) => {
                let id = window::Id::unique();
                self.surface = Some(id);
                cosmic::surface::surface_task(cosmic::surface::action::simple_layer_shell(
                    cosmic::surface::action::LiveSettings::default,
                    move || {
                        SctkLayerSurfaceSettings {
                        id,
                        layer: Layer::Overlay,
                        keyboard_interactivity: KeyboardInteractivity::None,
                        anchor: Anchor::BOTTOM,
                        output: IcedOutput::Active,
                        namespace: "cosmo-overlay".into(),
                        margin: cosmic::iced::platform_specific::runtime::wayland::layer_surface::IcedMargin {
                            bottom: BOTTOM_MARGIN,
                            ..Default::default()
                        },
                        // None: sized to the content by the autosize widget
                        // in `view_window`.
                        size: None,
                        // 0: stay clear of the dock's and panel's reserved
                        // space (-1 drew the card over the dock).
                        exclusive_zone: 0,
                        ..Default::default()
                    }
                    },
                    // No view here: the surface is drawn by `view_window`.
                    // (`app_layer_shell` without one falls back to the main
                    // `view`, which drew an empty 1×1 surface.)
                    None::<fn() -> Element<'static, cosmic::Action<Message>>>,
                ))
            }
            (false, Some(id)) => {
                self.surface = None;
                cosmic::surface::surface_task(cosmic::surface::action::destroy_layer_shell(id))
            }
            _ => Task::none(),
        }
    }
}

/// What a dismissal remembers: the state and the held actions shown.
fn dismiss_key(v: &View) -> (State, Vec<String>) {
    (v.state, v.holds.iter().map(|h| h.token.clone()).collect())
}

fn autosize_id() -> cosmic::widget::Id {
    static ID: std::sync::LazyLock<cosmic::widget::Id> =
        std::sync::LazyLock::new(|| cosmic::widget::Id::new("cosmo-overlay-card"));
    ID.clone()
}

/// The subscription: follow the daemon, apply bursts, emit only changes.
fn events() -> impl futures::Stream<Item = Message> {
    cosmic::iced::stream::channel(
        8,
        |mut out: futures::channel::mpsc::Sender<Message>| async move {
            use futures::SinkExt;
            let mut updates = cosmo_ipc::client::subscribe();
            let mut view = View::default();
            // While the card is up, check it against the daemon's status
            // every couple of seconds: a missed event mustn't strand it.
            let mut check = tokio::time::interval(std::time::Duration::from_secs(2));
            check.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                let changed = tokio::select! {
                    first = updates.recv() => {
                        let Some(first) = first else { return };
                        let mut burst = vec![first];
                        while let Ok(more) = updates.try_recv() {
                            burst.push(more);
                        }
                        apply_burst(&mut view, burst)
                    }
                    _ = check.tick(), if view.visible() => {
                        match cosmo_ipc::client::request(cosmo_ipc::Command::Status).await {
                            Ok(cosmo_ipc::Response::Status(s)) => view.reconcile(&s),
                            _ => false,
                        }
                    }
                };
                if changed
                    && out
                        .send(Message::View(Box::new(view.clone())))
                        .await
                        .is_err()
                {
                    return;
                }
            }
        },
    )
}

fn send(cmd: cosmo_ipc::Command) -> Task<cosmic::Action<Message>> {
    Task::perform(
        async move {
            match cosmo_ipc::client::request(cmd).await {
                Ok(cosmo_ipc::Response::Error { message }) => Err(message),
                Ok(_) => Ok(()),
                Err(e) => Err(e.to_string()),
            }
        },
        |r| cosmic::Action::App(Message::Sent(r)),
    )
}

/// The card: one line of state, then what goes with it.
fn card(v: &View) -> Element<'_, Message> {
    let (title, detail): (&str, String) = match v.state {
        State::Listening => ("Listening", v.transcript.clone()),
        State::Thinking => ("Thinking", v.transcript.clone()),
        State::Acting => (
            "Working",
            v.action.clone().unwrap_or_else(|| v.transcript.clone()),
        ),
        State::Waiting => ("Needs your confirmation", String::new()),
        State::Speaking => ("Speaking", v.reply.clone()),
        State::Idle => match &v.voice_render {
            Some((voice, done, total)) => {
                ("Preparing voice", format!("{voice}: {done} of {total}"))
            }
            None => ("", String::new()),
        },
    };
    let mut column = Column::new().spacing(8);
    let mut header = Row::new().spacing(12).align_y(Alignment::Center);
    if v.state == State::Listening {
        header = header.push(waveform(&v.levels));
    }
    header = header.push(text::title4(title).width(Length::Fill)).push(
        button::icon(widget::icon::from_name("window-close-symbolic")).on_press(Message::Dismiss),
    );
    column = column.push(header);
    if !detail.is_empty() {
        column = column.push(text::body(detail).width(Length::Fill));
    }
    for hold in &v.holds {
        column = column.push(
            Row::new()
                .spacing(8)
                .align_y(Alignment::Center)
                .push(text::body(hold.action.clone()).width(Length::Fill))
                .push(button::suggested("Confirm").on_press(Message::Confirm(hold.token.clone())))
                .push(button::standard("Cancel").on_press(Message::Cancel(hold.token.clone()))),
        );
    }
    container(column)
        .padding(16)
        .width(Length::Fixed(WIDTH as f32))
        .class(cosmic::theme::Container::Card)
        .into()
}

/// The mic level as a row of bars, newest on the right.
fn waveform(levels: &[f32]) -> Element<'_, Message> {
    let mut bars = Row::new()
        .spacing(2)
        .align_y(Alignment::Center)
        .height(Length::Fixed(24.0));
    for &l in levels.iter().skip(levels.len().saturating_sub(WAVE_POINTS)) {
        bars = bars.push(
            container(widget::Space::new())
                .width(Length::Fixed(2.0))
                .height(Length::Fixed(2.0 + 22.0 * l))
                .class(cosmic::theme::Container::Primary),
        );
    }
    bars.into()
}
