//! Unicode-safe text injection via a synthesised keymap over
//! `zwp_virtual_keyboard_v1`. It reaches every client, types arbitrary
//! Unicode, and never contends with a real IME.
//!
//! ## Invariant
//!
//! **NEVER bind `zwp_input_method_v2`.** A Wayland seat has a single
//! input-method slot; binding it while IBus holds it wedges keyboard input
//! session-wide on cosmic-comp (a smithay bug). Virtual keyboard only, no
//! exceptions — do not "fix" this later. See the comment at
//! [`Keyboard::connect`], the one place a future edit would reach.
//!
//! Mechanism lifted from `cosmic-voice/src/inject.rs` (MIT): build an XKB
//! keymap holding exactly the segment's distinct characters, upload it in a
//! sealed memfd, roundtrip so the compositor installs it, then tap press and
//! release per character. Text with too many distinct characters types in
//! segments with a keymap swap between them.

use std::os::fd::AsFd;
use std::os::fd::OwnedFd;

use anyhow::{Context, Result};
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
    zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
};

/// Distinct characters a single synthesised keymap can carry. XKB keycodes
/// run 8..=255 and we start at 9, so the true bound is 247; headroom is free.
const KEYMAP_BUDGET: usize = 200;

/// Wayland protocol state (virtual keyboard only — there is nothing else).
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

impl Dispatch<ZwpVirtualKeyboardManagerV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &ZwpVirtualKeyboardManagerV1,
        _: <ZwpVirtualKeyboardManagerV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpVirtualKeyboardV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &ZwpVirtualKeyboardV1,
        _: <ZwpVirtualKeyboardV1 as wayland_client::Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

/// A connected virtual keyboard on its own Wayland connection.
pub struct Keyboard {
    queue: wayland_client::EventQueue<State>,
    state: State,
    vk: ZwpVirtualKeyboardV1,
    epoch: std::time::Instant,
}

/// The compositor has no `zwp_virtual_keyboard_manager_v1` global.
#[derive(Debug, thiserror::Error)]
#[error("compositor lacks zwp_virtual_keyboard_manager_v1 — cannot type")]
pub struct NoVirtualKeyboard;

impl Keyboard {
    /// Connect to the compositor and create the virtual keyboard.
    ///
    /// # The invariant, at the only Wayland init site cosmo-type has
    ///
    /// We bind **only** `zwp_virtual_keyboard_manager_v1` and never
    /// `zwp_input_method_v2`. One slot per seat; binding while IBus holds it
    /// wedges keyboard input session-wide on cosmic-comp (smithay bug,
    /// blueprint v4 §3.4 / invariant #1). If a future refactor "unifies"
    /// injection paths and binds the IM here, it reintroduces the wedge —
    /// this comment is the tripwire.
    pub fn connect() -> Result<Self> {
        let conn = Connection::connect_to_env().context("connecting to the compositor")?;
        let (globals, queue) =
            registry_queue_init::<State>(&conn).context("initialising the registry")?;
        let qh = queue.handle();

        let seat: wl_seat::WlSeat = globals.bind(&qh, 1..=9, ()).context("binding wl_seat")?;
        let vk_mgr: ZwpVirtualKeyboardManagerV1 = globals
            .bind(&qh, 1..=1, ())
            .context("binding zwp_virtual_keyboard_manager_v1")?;
        let vk = vk_mgr.create_virtual_keyboard(&seat, &qh, ());

        let mut this = Self {
            queue,
            state: State,
            vk,
            epoch: std::time::Instant::now(),
        };
        // Ensure the initial registry burst is drained before first use.
        this.roundtrip()?;
        Ok(this)
    }

    fn roundtrip(&mut self) -> Result<()> {
        self.queue
            .roundtrip(&mut self.state)
            .context("wayland roundtrip")?;
        Ok(())
    }

    /// Types `text` verbatim through the virtual keyboard.
    ///
    /// Builds a keymap holding the segment's distinct characters, uploads
    /// it, waits for the compositor to install it, then taps the sequence.
    /// More distinct characters than one keymap holds ⇒ type in segments
    /// with a keymap swap between them.
    pub fn type_text(&mut self, text: &str) -> Result<()> {
        if text.is_empty() {
            return Ok(());
        }
        for segment in segment_by_distinct(text, KEYMAP_BUDGET) {
            self.type_segment(&segment)?;
        }
        Ok(())
    }

    fn type_segment(&mut self, text: &str) -> Result<()> {
        // Keycodes by first appearance. XKB code 9 is first; the wire
        // carries evdev codes, which are the XKB code minus 8.
        let mut order: Vec<char> = Vec::new();
        for ch in text.chars() {
            if !order.contains(&ch) {
                order.push(ch);
            }
        }

        let keymap = build_keymap(&order);
        let fd = upload_keymap(&keymap)?;
        self.vk.keymap(
            1, // XKB_KEYMAP_FORMAT_TEXT_V1
            fd.as_fd(),
            keymap.len() as u32 + 1,
        );
        // The keymap request carries no ack, so a roundtrip is the only way
        // to know the compositor installed it before keys start arriving.
        self.roundtrip()?;
        self.vk.modifiers(0, 0, 0, 0);

        for ch in text.chars() {
            let idx = order
                .iter()
                .position(|&c| c == ch)
                .expect("segment built from this text");
            let code = idx as u32 + 1; // evdev code; XKB sees +8 = 9..
            let t = self.epoch.elapsed().as_millis() as u32;
            self.vk.key(t, code, 1);
            self.vk.key(t, code, 0);
        }
        self.roundtrip()?;
        Ok(())
    }
}

/// Splits text into runs whose distinct-character count stays within budget.
pub fn segment_by_distinct(text: &str, budget: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut distinct: Vec<char> = Vec::new();

    for ch in text.chars() {
        if !distinct.contains(&ch) {
            if distinct.len() == budget {
                out.push(std::mem::take(&mut current));
                distinct.clear();
            }
            distinct.push(ch);
        }
        current.push(ch);
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// Renders an XKB keymap mapping keycode 9+n to the nth character.
fn build_keymap(chars: &[char]) -> String {
    use std::fmt::Write;

    let mut codes = String::new();
    let mut syms = String::new();
    for (i, &ch) in chars.iter().enumerate() {
        let code = i + 9;
        let _ = writeln!(codes, "\t\t<K{i}> = {code};");
        let _ = writeln!(syms, "\t\tkey <K{i}> {{ [ {} ] }};", keysym(ch));
    }

    format!(
        "xkb_keymap {{\n\
         \txkb_keycodes \"cosmo\" {{\n\
         \t\tminimum = 8;\n\
         \t\tmaximum = 255;\n\
         {codes}\
         \t}};\n\
         \txkb_types \"cosmo\" {{\n\
         \t\ttype \"ONE_LEVEL\" {{ modifiers = none; map[none] = Level1; }};\n\
         \t}};\n\
         \txkb_compatibility \"cosmo\" {{ }};\n\
         \txkb_symbols \"cosmo\" {{\n\
         {syms}\
         \t}};\n\
         }};\n"
    )
}

/// XKB keysym name for a character: `U<hex>` covers all of Unicode; control
/// characters get their named keysyms.
fn keysym(ch: char) -> String {
    match ch {
        '\n' => "Return".into(),
        '\t' => "Tab".into(),
        _ => format!("U{:04X}", ch as u32),
    }
}

/// Puts the keymap text, NUL-terminated, into a sealed memfd.
fn upload_keymap(keymap: &str) -> Result<OwnedFd> {
    use std::io::Write;

    let fd = rustix::fs::memfd_create("cosmo-keymap", rustix::fs::MemfdFlags::CLOEXEC)
        .context("memfd_create")?;
    let mut file = std::fs::File::from(fd);
    file.write_all(keymap.as_bytes())
        .context("writing keymap")?;
    file.write_all(&[0]).context("writing keymap terminator")?;
    Ok(file.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_respect_budget() {
        let text = "abcdefghij";
        let segs = segment_by_distinct(text, 4);
        // Distinct chars: a..j = 10 → segments of ≤4 distinct chars.
        assert!(segs.len() >= 3);
        // Reconstruction must be exact.
        let joined: String = segs.concat();
        assert_eq!(joined, text);
        for seg in &segs {
            let distinct = seg.chars().collect::<std::collections::HashSet<_>>();
            assert!(distinct.len() <= 4, "segment {seg} over budget");
        }
    }

    #[test]
    fn single_segment_for_small_text() {
        let segs = segment_by_distinct("hello", 200);
        assert_eq!(segs, vec!["hello".to_string()]);
    }

    #[test]
    fn keymap_carries_all_chars() {
        let chars: Vec<char> = "aÉ7\n".chars().collect();
        let keymap = build_keymap(&chars);
        assert!(keymap.contains("<K0> = 9;"));
        assert!(keymap.contains("[ U0061 ]"));
        assert!(keymap.contains("[ U00C9 ]"));
        assert!(keymap.contains("[ U0037 ]"));
        assert!(keymap.contains("[ Return ]"));
        // Every keycode distinct and within XKB bounds.
        assert!(keymap.contains("<K3> = 12;"));
        assert!(keymap.starts_with("xkb_keymap {"));
    }

    #[test]
    fn keymap_memfd_is_wellformed() {
        let keymap = build_keymap(&['x']);
        let fd = upload_keymap(&keymap).unwrap();
        // Read it back via /proc/self/fd to prove it is a valid sealed fd
        // carrying exactly the keymap plus its NUL terminator.
        use std::os::fd::AsRawFd;
        let path = format!("/proc/self/fd/{}", fd.as_raw_fd());
        let bytes = std::fs::read(path).expect("read memfd");
        assert_eq!(bytes.last(), Some(&0));
        assert!(bytes.starts_with(b"xkb_keymap {"));
    }
}
