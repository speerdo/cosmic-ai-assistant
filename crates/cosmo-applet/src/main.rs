//! `cosmo-applet`: the COSMIC panel applet (phase-6 spec §6.5). A thin
//! libcosmic client over the control socket.
//!
//! ## Invariant
//!
//! **The panel spawns one applet process per output. The daemon owns the
//! mic, the hotkey and the models, always.** The applet owns nothing: it
//! shows the daemon's state and sends it commands. Two applets on two
//! monitors are two views of one cosmo. See the `cosmo-daemon` crate docs
//! before being tempted to move state here.
//!
//! ## Licence
//!
//! libcosmic's `applet` feature links two GPL-3.0 crates from cosmic-panel
//! (every COSMIC applet does), so this binary, as distributed, carries
//! GPL-3.0 terms; its source is MIT like the rest of cosmo. The user chose
//! this on 2026-09-30. Nothing else in cosmo links them (`THIRD_PARTY.md`).

use cosmic::Element;
use cosmic::app::{Core, Task};
use cosmic::iced::widget::{Column, Row};
use cosmic::iced::window::Id;
use cosmic::iced::{Alignment, Length, Rectangle, Subscription};
use cosmic::surface::action::{app_popup, destroy_popup};
use cosmic::widget::{self, button, text, toggler};
use cosmo_ipc::client::Update;
use cosmo_ipc::{Command, DoctorReport, Event, Response, State, VoiceInfo};
use libcosmic as cosmic;

fn main() -> cosmic::iced::Result {
    cosmic::applet::run::<Applet>(())
}

#[derive(Default)]
struct Applet {
    core: Core,
    popup: Option<Id>,
    connected: bool,
    state: State,
    paused: bool,
    /// Holds awaiting a confirm (their count drives the icon).
    holds: usize,
    voices: Option<VoiceList>,
    render: Option<(String, u32, u32)>,
    doctor: Option<DoctorReport>,
    error: Option<String>,
}

#[derive(Debug, Clone)]
struct VoiceList {
    provider: String,
    active: Option<String>,
    voices: Vec<VoiceInfo>,
}

#[derive(Debug, Clone)]
enum Message {
    Update(Update),
    Surface(cosmic::surface::Action<Message>),
    PopupClosed(Id),
    SetPaused(bool),
    Preview(String),
    UseVoice(String),
    Reply(Result<Response, String>),
}

impl cosmic::Application for Applet {
    type Executor = cosmic::executor::Default;
    type Flags = ();
    type Message = Message;
    const APP_ID: &'static str = "io.github.speerdo.CosmoApplet";

    fn core(&self) -> &Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut Core {
        &mut self.core
    }

    fn init(core: Core, _: ()) -> (Self, Task<Message>) {
        (
            Self {
                core,
                ..Self::default()
            },
            Task::none(),
        )
    }

    fn on_close_requested(&self, id: Id) -> Option<Message> {
        Some(Message::PopupClosed(id))
    }

    fn subscription(&self) -> Subscription<Message> {
        Subscription::run(updates)
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Update(Update::Connected) => {
                self.connected = true;
                // Everything the popup shows, fetched up front so opening it
                // is instant.
                return Task::batch([
                    request(Command::Status),
                    request(Command::VoiceList { provider: None }),
                    request(Command::Doctor),
                ]);
            }
            Message::Update(Update::Disconnected) => {
                *self = Self {
                    core: std::mem::take(&mut self.core),
                    popup: self.popup,
                    ..Self::default()
                };
            }
            Message::Update(Update::Event(e)) => match e {
                Event::State { state } => self.state = state,
                Event::Held { .. } => self.holds += 1,
                Event::HoldResolved { .. } => self.holds = self.holds.saturating_sub(1),
                Event::VoiceCacheProgress {
                    voice, done, total, ..
                } => self.render = Some((voice, done, total)),
                Event::VoiceCacheDone { .. } => {
                    self.render = None;
                    return request(Command::VoiceList { provider: None });
                }
                _ => {}
            },
            Message::Surface(a) => return cosmic::task::message(cosmic::Action::Surface(a)),
            Message::PopupClosed(id) => {
                if self.popup == Some(id) {
                    self.popup = None;
                }
            }
            Message::SetPaused(paused) => {
                if paused != self.paused {
                    return request(Command::Toggle);
                }
            }
            Message::Preview(voice) => {
                return request(Command::VoicePreview {
                    provider: None,
                    voice,
                });
            }
            Message::UseVoice(voice) => {
                return request(Command::VoiceSet {
                    provider: None,
                    voice,
                });
            }
            Message::Reply(Ok(response)) => {
                self.error = None;
                match response {
                    Response::Status(s) => {
                        self.paused = s.paused;
                        self.state = s.state;
                        self.holds = s.pending_holds.len();
                    }
                    Response::Toggled { paused } => self.paused = paused,
                    Response::Voices {
                        provider,
                        active,
                        voices,
                    } => {
                        self.voices = Some(VoiceList {
                            provider,
                            active,
                            voices,
                        })
                    }
                    Response::Doctor(report) => self.doctor = Some(report),
                    Response::VoiceSet { .. } => {
                        return request(Command::VoiceList { provider: None });
                    }
                    Response::Error { message } => self.error = Some(message),
                    _ => {}
                }
            }
            Message::Reply(Err(e)) => self.error = Some(e),
        }
        Task::none()
    }

    fn view(&self) -> Element<'_, Message> {
        let have_popup = self.popup;
        let button = self
            .core
            .applet
            .icon_button(self.icon())
            .on_press_with_rectangle(move |offset, bounds| match have_popup {
                Some(id) => Message::Surface(destroy_popup(id)),
                None => Message::Surface(app_popup::<Applet>(
                    |_| Default::default(),
                    move |state: &mut Applet| {
                        let id = Id::unique();
                        state.popup = Some(id);
                        let mut settings = state.core.applet.get_popup_settings(
                            state.core.main_window_id().unwrap(),
                            id,
                            None,
                            None,
                            None,
                        );
                        settings.positioner.anchor_rect = Rectangle {
                            x: (bounds.x - offset.x) as i32,
                            y: (bounds.y - offset.y) as i32,
                            width: bounds.width as i32,
                            height: bounds.height as i32,
                        };
                        settings
                    },
                    Some(Box::new(|state: &Applet| {
                        Element::from(state.core.applet.popup_container(state.popup_view()))
                            .map(cosmic::Action::App)
                    })),
                )),
            });
        Element::from(self.core.applet.applet_tooltip::<Message>(
            button,
            self.summary(),
            self.popup.is_some(),
            Message::Surface,
            None,
        ))
    }

    fn view_window(&self, _: Id) -> Element<'_, Message> {
        // The popup draws through its own view (above).
        widget::Space::new().into()
    }
}

impl Applet {
    fn icon(&self) -> &'static str {
        match (self.connected, self.paused, self.state) {
            (false, _, _) => "microphone-disabled-symbolic",
            (true, true, _) => "media-playback-pause-symbolic",
            (true, false, State::Listening) => "audio-input-microphone-high-symbolic",
            _ if self.holds > 0 => "dialog-question-symbolic",
            _ => "audio-input-microphone-symbolic",
        }
    }

    fn summary(&self) -> String {
        if !self.connected {
            return "cosmo isn't running".into();
        }
        if self.paused {
            return "cosmo is paused".into();
        }
        match self.state {
            State::Idle if self.holds > 0 => "cosmo is waiting for your confirmation".into(),
            State::Idle => "cosmo is ready: hold Right Ctrl to talk".into(),
            State::Listening => "cosmo is listening".into(),
            State::Thinking => "cosmo is thinking".into(),
            State::Acting => "cosmo is working".into(),
            State::Waiting => "cosmo is waiting for your confirmation".into(),
            State::Speaking => "cosmo is speaking".into(),
        }
    }

    fn popup_view(&self) -> Element<'_, Message> {
        let mut col = Column::new()
            .spacing(12)
            .padding(12)
            .width(Length::Fixed(360.0));
        col = col.push(text::title4(self.summary()));
        if !self.connected {
            col = col.push(text::body(
                "Start it with `systemctl --user start cosmo`, or run `cosmod`.",
            ));
            return col.into();
        }
        col = col.push(
            Row::new()
                .align_y(Alignment::Center)
                .push(text::body("Listening").width(Length::Fill))
                .push(toggler(!self.paused).on_toggle(|on| Message::SetPaused(!on))),
        );
        if let Some(err) = &self.error {
            col = col.push(text::caption(err.clone()));
        }

        // Voices, grouped by accent.
        col = col.push(text::heading("Voice"));
        match &self.voices {
            None => col = col.push(text::caption("Loading voices…")),
            Some(list) => {
                if let Some((voice, done, total)) = &self.render {
                    col = col.push(text::caption(format!(
                        "Preparing {voice}: {done} of {total} phrases"
                    )));
                }
                let mut voices = list.voices.clone();
                voices.sort_by(|a, b| (&a.accent, &a.label).cmp(&(&b.accent, &b.label)));
                let mut accent = String::new();
                let mut rows = Column::new().spacing(4);
                for v in voices {
                    if v.accent != accent {
                        accent.clone_from(&v.accent);
                        rows = rows.push(text::caption_heading(accent.clone()));
                    }
                    let active = list.active.as_deref() == Some(v.id.as_str());
                    let mut r = Row::new()
                        .spacing(8)
                        .align_y(Alignment::Center)
                        .push(
                            text::body(if active {
                                format!("{} ✓", v.label)
                            } else {
                                v.label.clone()
                            })
                            .width(Length::Fill),
                        )
                        .push(button::text("Preview").on_press(Message::Preview(v.id.clone())));
                    if !active {
                        r = r.push(button::text("Use").on_press(Message::UseVoice(v.id.clone())));
                    }
                    rows = rows.push(r);
                }
                col = col.push(text::caption(format!("Provider: {}", list.provider)));
                col = col.push(widget::scrollable(rows).height(Length::Fixed(240.0)));
            }
        }

        // doctor, summarised.
        if let Some(report) = &self.doctor {
            let failing: Vec<&str> = report
                .checks
                .iter()
                .filter(|c| !c.ok)
                .map(|c| c.name.as_str())
                .collect();
            let line = if failing.is_empty() {
                format!("All {} checks pass", report.checks.len())
            } else {
                format!("Needs attention: {}", failing.join(", "))
            };
            col = col.push(text::caption(line));
        }
        col.into()
    }
}

/// Follow the daemon, passing on only what the applet shows: its panel
/// icon mustn't redraw 20 times a second for audio levels.
fn updates() -> impl futures::Stream<Item = Message> {
    cosmic::iced::stream::channel(
        8,
        |mut out: futures::channel::mpsc::Sender<Message>| async move {
            use futures::SinkExt;
            let mut rx = cosmo_ipc::client::subscribe();
            while let Some(u) = rx.recv().await {
                let wanted = match &u {
                    Update::Event(e) => matches!(
                        e,
                        Event::State { .. }
                            | Event::Held { .. }
                            | Event::HoldResolved { .. }
                            | Event::VoiceCacheProgress { .. }
                            | Event::VoiceCacheDone { .. }
                    ),
                    _ => true,
                };
                if wanted && out.send(Message::Update(u)).await.is_err() {
                    return;
                }
            }
        },
    )
}

fn request(cmd: Command) -> Task<Message> {
    Task::perform(
        async move {
            cosmo_ipc::client::request(cmd)
                .await
                .map_err(|e| e.to_string())
        },
        |r| cosmic::Action::App(Message::Reply(r)),
    )
}
