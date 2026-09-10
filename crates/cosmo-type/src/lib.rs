//! Unicode-safe text injection via a synthesised keymap over
//! `zwp_virtual_keyboard_v1`. It reaches every client, types arbitrary
//! Unicode, and never contends with a real IME.
//!
//! ## Invariant
//!
//! **NEVER bind `zwp_input_method_v2`.** A Wayland seat has a single
//! input-method slot; binding it while IBus holds it wedges keyboard input
//! session-wide on cosmic-comp (a smithay bug). Virtual keyboard only, no
//! exceptions — do not "fix" this later.
