//! Global trigger key read straight off evdev, with udev hotplug handling.
//!
//! ## Invariants
//!
//! - `EVIOCGRAB` grabs the keyboard and `EVIOCSMASK` restricts this process to
//!   the trigger keycode, with `MSC_SCAN` filtered. Only the trigger key is
//!   ever read or delivered here — this daemon is not a keylogger.
//! - The grab must only be held while it is needed, so the trigger key does
//!   not leak to the focused application while held.
//! - No root, no `input` group: logind's `uaccess` ACL is sufficient.
