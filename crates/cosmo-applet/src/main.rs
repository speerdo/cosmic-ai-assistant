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
use cosmo_ipc::{
    Command, Connect, DoctorReport, Event, ProviderInfo, ReasoningInfo, Redacted, Response, State,
    VoiceInfo,
};
use libcosmic as cosmic;

/// cosmo's logo (assets/icons), in the popup's header.
const LOGO: &[u8] = include_bytes!("../../../assets/icons/io.github.speerdo.Cosmo.svg");

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
    /// The voice list is folded away until asked for: one row shows the
    /// active voice, and the full list (40-odd rows) opens on a click.
    voices_open: bool,
    /// The readiness details, folded like the voices.
    status_open: bool,
    /// Reasoning providers and their connection state.
    reasoning: Option<ReasoningInfo>,
    reasoning_open: bool,
    /// The provider whose models are listed. `None` follows the one in use;
    /// an empty name means all are folded.
    provider_open: Option<String>,
    /// The not-yet-connected providers, folded until asked for.
    add_open: bool,
    /// The provider whose key is being typed, and the key so far.
    key_entry: Option<String>,
    key_text: Redacted,
    /// The last sign-in or switch outcome, shown under the list.
    reasoning_note: Option<String>,
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
    ToggleVoices,
    ToggleStatus,
    ToggleReasoning,
    ToggleProvider(String),
    ToggleAdd,
    /// Reason with this provider and model ("" = its default).
    UseProvider(String, String),
    SignIn(String),
    /// Show the key field for this provider.
    AddKey(String),
    KeyInput(Redacted),
    SaveKey,
    OpenUrl(String),
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
                    request(Command::Reasoning),
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
                Event::SignIn {
                    provider,
                    ok,
                    detail,
                } => {
                    self.reasoning_note = Some(detail);
                    let mut next = vec![request(Command::Reasoning), request(Command::Doctor)];
                    if ok && !self.active_connected() {
                        next.push(request(Command::ReasoningSet {
                            provider,
                            model: String::new(),
                        }));
                    }
                    return Task::batch(next);
                }
                _ => {}
            },
            Message::Surface(a) => {
                let surface = cosmic::task::message(cosmic::Action::Surface(a));
                // Opening the popup: readiness may have changed since.
                if self.popup.is_none() && self.connected {
                    return Task::batch([surface, request(Command::Doctor)]);
                }
                return surface;
            }
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
            Message::ToggleVoices => self.voices_open = !self.voices_open,
            Message::ToggleStatus => self.status_open = !self.status_open,
            Message::ToggleReasoning => {
                self.reasoning_open = !self.reasoning_open;
                if self.reasoning_open {
                    // Connection state may have changed (a local server
                    // started, a key added from the terminal).
                    return request(Command::Reasoning);
                }
            }
            Message::ToggleProvider(name) => {
                let shown = self
                    .provider_open
                    .as_deref()
                    .or(self.reasoning.as_ref().map(|r| r.active.as_str()));
                self.provider_open = Some(if shown == Some(name.as_str()) {
                    String::new()
                } else {
                    name
                });
            }
            Message::ToggleAdd => self.add_open = !self.add_open,
            Message::UseProvider(provider, model) => {
                self.reasoning_note = None;
                return request(Command::ReasoningSet { provider, model });
            }
            Message::SignIn(provider) => {
                self.reasoning_note = Some("Waiting for your browser…".into());
                return request(Command::SignInStart { provider });
            }
            Message::AddKey(provider) => {
                self.key_text = Redacted(String::new());
                self.key_entry = if self.key_entry.as_deref() == Some(provider.as_str()) {
                    None
                } else {
                    Some(provider)
                };
            }
            Message::KeyInput(k) => self.key_text = k,
            Message::SaveKey => {
                let (Some(provider), key) = (
                    self.key_entry.clone(),
                    std::mem::replace(&mut self.key_text, Redacted(String::new())),
                ) else {
                    return Task::none();
                };
                if key.0.trim().is_empty() {
                    return Task::none();
                }
                return request(Command::StoreKey { provider, key });
            }
            Message::OpenUrl(url) => open_url(&url),
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
                    Response::Reasoning(info) => self.reasoning = Some(info),
                    Response::ReasoningSet { provider, model } => {
                        let label = self.provider_label(&provider);
                        self.reasoning_note = Some(format!("Now using {label} · {model}"));
                        return Task::batch([
                            request(Command::Reasoning),
                            request(Command::Doctor),
                        ]);
                    }
                    Response::SignInUrl { url, .. } => open_url(&url),
                    Response::KeyStored { provider } => {
                        self.key_entry = None;
                        let label = self.provider_label(&provider);
                        self.reasoning_note = Some(format!("{label} key saved"));
                        let mut next = vec![request(Command::Reasoning), request(Command::Doctor)];
                        if !self.active_connected() {
                            next.push(request(Command::ReasoningSet {
                                provider,
                                model: String::new(),
                            }));
                        }
                        return Task::batch(next);
                    }
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

    /// The state in a few words, under the name in the popup.
    fn status(&self) -> &'static str {
        if !self.connected {
            return "Not running";
        }
        if self.paused {
            return "Paused";
        }
        match self.state {
            State::Idle if self.holds > 0 => "Waiting for your confirmation",
            State::Idle => "Ready · hold Right Ctrl to talk",
            State::Listening => "Listening…",
            State::Thinking => "Thinking…",
            State::Acting => "Working…",
            State::Waiting => "Waiting for your confirmation",
            State::Speaking => "Speaking…",
        }
    }

    /// The panel tooltip.
    fn summary(&self) -> String {
        format!("Cosmo — {}", self.status())
    }

    /// Logo, name and state: the top of the popup.
    fn header(&self) -> Element<'_, Message> {
        let logo = widget::icon(widget::icon::from_svg_bytes(LOGO)).size(40);
        Row::new()
            .spacing(12)
            .align_y(Alignment::Center)
            .push(logo)
            .push(
                Column::new()
                    .spacing(2)
                    .push(text::title4("Cosmo"))
                    .push(text::caption(self.status())),
            )
            .into()
    }

    fn provider_label(&self, name: &str) -> String {
        self.reasoning
            .as_ref()
            .and_then(|r| r.providers.iter().find(|p| p.name == name))
            .map_or_else(|| name.to_owned(), |p| p.label.clone())
    }

    /// Whether the provider in use can answer now.
    fn active_connected(&self) -> bool {
        self.reasoning.as_ref().is_some_and(|r| {
            r.providers
                .iter()
                .any(|p| p.name == r.active && p.connected)
        })
    }

    /// Reasoning: the provider in use on one row; opened, every provider
    /// with what it takes to connect it (sign in, a key, a local server).
    fn reasoning_section(&self) -> Element<'_, Message> {
        let mut col = Column::new().spacing(8);
        let Some(info) = &self.reasoning else {
            return col.push(text::caption("Checking providers…")).into();
        };
        let current = match info.providers.iter().find(|p| p.name == info.active) {
            Some(p) if p.connected => format!("{} · {}", short_label(p), info.model),
            Some(p) => format!("{} · not connected", short_label(p)),
            None => format!("{} · unknown provider", info.active),
        };
        let chevron = if self.reasoning_open {
            "pan-down-symbolic"
        } else {
            "pan-end-symbolic"
        };
        col = col.push(
            cosmic::applet::menu_button(
                Row::new()
                    .spacing(8)
                    .align_y(Alignment::Center)
                    .push(
                        Column::new()
                            .spacing(2)
                            .width(Length::Fill)
                            .push(text::body("Reasoning"))
                            .push(text::caption(current)),
                    )
                    .push(widget::icon::from_name(chevron).size(16).icon()),
            )
            .on_press(Message::ToggleReasoning),
        );
        if !self.reasoning_open {
            return col.into();
        }
        // Connected providers first, each its own card with its models; the
        // rest are one fold away, so the list isn't a wall of sign-ins.
        let (connected, others): (Vec<_>, Vec<_>) =
            info.providers.iter().partition(|p| p.connected);
        let shown = self.provider_open.as_deref().unwrap_or(&info.active);
        let mut rows = Column::new().spacing(12);
        if connected.is_empty() {
            rows = rows.push(text::caption(
                "Nothing connected yet. Add a provider below.",
            ));
        }
        for p in connected {
            rows = rows.push(card(self.connected_card(info, p, shown == p.name)));
        }
        if !others.is_empty() {
            let chevron = if self.add_open {
                "pan-down-symbolic"
            } else {
                "pan-end-symbolic"
            };
            rows = rows.push(
                cosmic::applet::menu_button(
                    Row::new()
                        .spacing(8)
                        .align_y(Alignment::Center)
                        .push(text::body("Add a provider").width(Length::Fill))
                        .push(widget::icon::from_name(chevron).size(16).icon()),
                )
                .on_press(Message::ToggleAdd),
            );
            if self.add_open {
                for p in others {
                    rows = rows.push(card(self.connect_card(p)));
                }
            }
        }
        col =
            col.push(widget::scrollable(rows.padding([0, 12, 0, 0])).height(Length::Fixed(320.0)));
        if let Some(note) = &self.reasoning_note {
            col = col.push(text::caption(note.clone()));
        }
        col.into()
    }

    /// A connected provider: its name and state on one row (a click folds
    /// its models), then every model it offers, any of them usable. All the
    /// connected providers are switchable at any time; one is in use.
    fn connected_card<'a>(
        &'a self,
        info: &'a ReasoningInfo,
        p: &'a ProviderInfo,
        open: bool,
    ) -> Element<'a, Message> {
        let active = p.name == info.active;
        let count = p.models.len() + p.remote_models.len();
        let status = match (active, p.connect) {
            (true, _) => format!("In use · {}", info.model),
            (false, Connect::Local) => format!(
                "Running on this computer · {}",
                plural(count, "model", "models")
            ),
            (false, _) if count > 0 => format!("Connected · {}", plural(count, "model", "models")),
            (false, _) => "Connected".into(),
        };
        let chevron = if open {
            "pan-down-symbolic"
        } else {
            "pan-end-symbolic"
        };
        let mut col = Column::new().spacing(8).push(
            cosmic::applet::menu_button(
                Row::new()
                    .spacing(8)
                    .align_y(Alignment::Center)
                    .push(
                        Column::new()
                            .spacing(2)
                            .width(Length::Fill)
                            .push(text::body(p.label.clone()))
                            .push(text::caption(status)),
                    )
                    .push(widget::icon::from_name(chevron).size(16).icon()),
            )
            .on_press(Message::ToggleProvider(p.name.clone())),
        );
        if !open {
            return col.into();
        }
        let mut models = Column::new().spacing(4);
        if p.connect == Connect::Local && count == 0 {
            models = models.push(text::caption(format!(
                "No models on this computer yet: `ollama pull {}`",
                p.default_model
            )));
        }
        // Where the provider lists none, its default is the one choice.
        if p.connect != Connect::Local && count == 0 {
            models = models.push(model_row(info, p, &p.default_model));
        }
        for m in &p.models {
            models = models.push(model_row(info, p, m));
        }
        // A local server's models that run on a cloud, said plainly.
        if !p.remote_models.is_empty() {
            models = models
                .push(widget::divider::horizontal::light())
                .push(text::caption(
                    "Through Ollama's cloud (runs on ollama.com, not private):",
                ));
            for m in &p.remote_models {
                models = models.push(model_row(info, p, m));
            }
        }
        col = col.push(widget::divider::horizontal::light()).push(models);
        if p.connect != Connect::Local {
            col = col.push(button::text("Replace key").on_press(Message::AddKey(p.name.clone())));
            col = self.key_form(col, p);
        }
        if let Some(note) = &p.note
            && p.connect != Connect::Local
        {
            col = col.push(text::caption(note.clone()));
        }
        col.into()
    }

    /// A provider that isn't connected yet, and the one step that connects it.
    fn connect_card<'a>(&'a self, p: &'a ProviderInfo) -> Element<'a, Message> {
        let (status, action): (&str, Element<'a, Message>) = match p.connect {
            Connect::Browser => (
                "Sign in with your browser",
                button::suggested("Sign in")
                    .on_press(Message::SignIn(p.name.clone()))
                    .into(),
            ),
            Connect::Key => (
                "Needs an API key",
                button::text("Add key")
                    .on_press(Message::AddKey(p.name.clone()))
                    .into(),
            ),
            Connect::Local => (
                "No local server found",
                button::text("Get Ollama")
                    .on_press(Message::OpenUrl(p.key_page.clone()))
                    .into(),
            ),
        };
        let col = Column::new().spacing(8).push(
            Row::new()
                .spacing(8)
                .align_y(Alignment::Center)
                .push(
                    Column::new()
                        .spacing(2)
                        .width(Length::Fill)
                        .push(text::body(p.label.clone()))
                        .push(text::caption(status)),
                )
                .push(action),
        );
        let col = self.key_form(col, p);
        match &p.note {
            Some(note) => col.push(text::caption(note.clone())),
            None => col,
        }
        .into()
    }

    /// The key field under a provider, while its key is being typed.
    fn key_form<'a>(
        &'a self,
        col: Column<'a, Message, cosmic::Theme>,
        p: &'a ProviderInfo,
    ) -> Column<'a, Message, cosmic::Theme> {
        if self.key_entry.as_deref() != Some(p.name.as_str()) {
            return col;
        }
        col.push(
            widget::secure_input("Paste the API key", self.key_text.0.as_str(), None, true)
                .on_input(|k| Message::KeyInput(Redacted(k)))
                .on_submit(|_| Message::SaveKey),
        )
        .push(
            Row::new()
                .spacing(8)
                .push(button::text("Get a key").on_press(Message::OpenUrl(p.key_page.clone())))
                .push(widget::Space::new().width(Length::Fill))
                .push(button::suggested("Save").on_press(Message::SaveKey)),
        )
    }

    /// Readiness (the daemon's `doctor`): one row saying whether all is
    /// well, opening onto what isn't and how to fix it.
    fn status_section(&self) -> Element<'_, Message> {
        let mut col = Column::new().spacing(8);
        let Some(report) = &self.doctor else {
            return col.push(text::caption("Checking…")).into();
        };
        let failing = report.checks.iter().filter(|c| !c.ok).count();
        let notes = report.checks.iter().filter(|c| c.ok && c.warn).count();
        let plural =
            |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
        let (icon, line) = match (failing, notes) {
            (0, 0) => ("emblem-ok-symbolic", "Everything's working".to_owned()),
            (0, n) => (
                "dialog-information-symbolic",
                format!("Everything's working · {}", plural(n, "note", "notes")),
            ),
            (n, _) => (
                "dialog-warning-symbolic",
                format!("{} attention", plural(n, "thing needs", "things need")),
            ),
        };
        let chevron = if self.status_open {
            "pan-down-symbolic"
        } else {
            "pan-end-symbolic"
        };
        col = col.push(
            cosmic::applet::menu_button(
                Row::new()
                    .spacing(8)
                    .align_y(Alignment::Center)
                    .push(widget::icon::from_name(icon).size(16).icon())
                    .push(text::body(line).width(Length::Fill))
                    .push(widget::icon::from_name(chevron).size(16).icon()),
            )
            .on_press(Message::ToggleStatus),
        );
        if !self.status_open {
            return col.into();
        }
        // Problems first, then notes; what's fine isn't listed.
        let mut shown: Vec<_> = report.checks.iter().filter(|c| !c.ok || c.warn).collect();
        shown.sort_by_key(|c| c.ok);
        for c in shown {
            let icon = if c.ok {
                "dialog-information-symbolic"
            } else {
                "dialog-warning-symbolic"
            };
            col = col.push(
                Row::new()
                    .spacing(8)
                    .push(widget::icon::from_name(icon).size(16).icon())
                    .push(
                        Column::new()
                            .spacing(2)
                            .width(Length::Fill)
                            .push(text::body(check_label(&c.name)))
                            .push(text::caption(c.detail.clone())),
                    ),
            );
        }
        col.push(text::caption("Full report: cosmo doctor")).into()
    }

    /// The voice section: the active voice on one row, the whole list
    /// only when opened.
    fn voice_section(&self) -> Element<'_, Message> {
        let mut col = Column::new().spacing(8);
        let Some(list) = &self.voices else {
            return col.push(text::caption("Loading voices…")).into();
        };
        let active = list
            .active
            .as_deref()
            .and_then(|id| list.voices.iter().find(|v| v.id == id));
        let current = match active {
            Some(v) => format!("{} · {}", v.label, v.accent),
            None => "None chosen".into(),
        };
        let chevron = if self.voices_open {
            "pan-down-symbolic"
        } else {
            "pan-end-symbolic"
        };
        col = col.push(
            cosmic::applet::menu_button(
                Row::new()
                    .spacing(8)
                    .align_y(Alignment::Center)
                    .push(
                        Column::new()
                            .spacing(2)
                            .width(Length::Fill)
                            .push(text::body("Voice"))
                            .push(text::caption(current)),
                    )
                    .push(widget::icon::from_name(chevron).size(16).icon()),
            )
            .on_press(Message::ToggleVoices),
        );
        if let Some((voice, done, total)) = &self.render {
            col = col.push(text::caption(format!(
                "Preparing {voice}: {done} of {total} phrases"
            )));
        }
        if !self.voices_open {
            return col.into();
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
            let is_active = list.active.as_deref() == Some(v.id.as_str());
            let mut r = Row::new()
                .spacing(8)
                .align_y(Alignment::Center)
                .push(
                    text::body(if is_active {
                        format!("{} ✓", v.label)
                    } else {
                        v.label.clone()
                    })
                    .width(Length::Fill),
                )
                .push(button::text("Preview").on_press(Message::Preview(v.id.clone())));
            if !is_active {
                r = r.push(button::text("Use").on_press(Message::UseVoice(v.id.clone())));
            }
            rows = rows.push(r);
        }
        col.push(text::caption(format!("Provider: {}", list.provider)))
            .push(widget::scrollable(rows).height(Length::Fixed(240.0)))
            .into()
    }

    fn popup_view(&self) -> Element<'_, Message> {
        let mut col = Column::new()
            .spacing(12)
            .padding(12)
            .width(Length::Fixed(360.0));
        col = col.push(self.header());
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

        col = col.push(widget::divider::horizontal::default());
        col = col.push(self.reasoning_section());
        col = col.push(widget::divider::horizontal::default());
        col = col.push(self.voice_section());
        col = col.push(widget::divider::horizontal::default());

        col = col.push(self.status_section());
        col.into()
    }
}

/// One of a local server's models, with "Use" (or ✓ when in use).
fn model_row<'a>(info: &ReasoningInfo, p: &ProviderInfo, m: &str) -> Element<'a, Message> {
    let in_use = p.name == info.active && m == info.model;
    let pick: Element<'a, Message> = if in_use {
        text::caption("✓").into()
    } else {
        button::text("Use")
            .on_press(Message::UseProvider(p.name.clone(), m.to_owned()))
            .into()
    };
    Row::new()
        .spacing(8)
        .align_y(Alignment::Center)
        .push(text::caption(m.to_owned()).width(Length::Fill))
        .push(pick)
        .into()
}

/// A bordered, padded block: what separates one provider from the next.
fn card<'a>(content: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    widget::container(content)
        .padding(12)
        .width(Length::Fill)
        .class(cosmic::theme::Container::Card)
        .into()
}

/// A provider's name without its parenthetical ("Local (on this
/// computer)" → "Local").
fn short_label(p: &ProviderInfo) -> &str {
    p.label.split(" (").next().unwrap_or(&p.label)
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// Open a link in the user's browser.
fn open_url(url: &str) {
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
}

/// A `doctor` check's name, as a person would say it.
fn check_label(name: &str) -> String {
    match name {
        "config" => "Settings file",
        "control socket" => "Daemon",
        "lock policy" => "Screen lock",
        "api key" => "API key",
        "reasoning" => "Reasoning model",
        "agent (MCP)" => "Desktop control",
        "speech" => "Voice",
        "ears" => "Microphone and hotkey",
        "token use" => "Token use",
        other => return other.to_owned(),
    }
    .to_owned()
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
                            | Event::SignIn { .. }
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
