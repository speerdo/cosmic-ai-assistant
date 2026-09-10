//! Phase 0 probe: trigger key straight off evdev with `EVIOCGRAB` +
//! `EVIOCSMASK`.
//!
//! Mirrors cosmic-voice's `hotkey.rs` (MIT) ioctl semantics. Verifies:
//!
//! 1. press **and** release events arrive for the trigger code,
//! 2. nothing else is ever delivered to this process (mask proof),
//! 3. the key does not leak to the focused app while held (grab proof — the
//!    grab is taken on trigger press and released on release).
//!
//! Usage:
//!   phase0_evdev --list
//!   phase0_evdev [--device /dev/input/by-id/...] [--code 183] [--secs 20]
//!
//! Leak check while it runs: focus a text editor, hold the trigger (nothing
//! should type), keep holding and mash letter keys (they should also not
//! appear, because the grab is active for the duration of the hold), then
//! release.
//!
//! No root, no `input` group — logind `uaccess` ACLs only. The grab is tied to
//! this file descriptor, so closing it (including on crash or kill) releases
//! the keyboard back to the session automatically.

#![allow(unsafe_code)] // ioctl is the entire point of this probe

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const EV_SYN: u16 = 0x00;
const EV_KEY: u16 = 0x01;
const EV_MSC: u16 = 0x04;

/// `_IOW('E', 0x90, int)`.
const EVIOCGRAB: u64 = (1 << 30) | (4 << 16) | ((b'E' as u64) << 8) | 0x90;
/// `_IOW('E', 0x93, struct input_mask)` — 16 bytes.
const EVIOCSMASK: u64 = (1 << 30) | (16 << 16) | ((b'E' as u64) << 8) | 0x93;
/// `EVIOCGBIT(EV_KEY, 96)` — device key-capability bitmap, `(KEY_MAX+1)/8`.
const EVIOCGBIT_KEY: u64 = (2 << 30) | (96 << 16) | ((b'E' as u64) << 8) | (0x20 + EV_KEY as u64);

/// Trigger default: `KEY_F13`.
const KEY_F13: u16 = 183;

const KEY_NAMES: &[(u16, &str)] = &[
    (70, "ScrollLock"),
    (99, "PrintScreen"),
    (119, "Pause"),
    (183, "F13"),
    (184, "F14"),
    (185, "F15"),
    (186, "F16"),
    (187, "F17"),
    (188, "F18"),
    (189, "F19"),
    (190, "F20"),
    (191, "F21"),
    (192, "F22"),
    (193, "F23"),
    (194, "F24"),
];

#[repr(C)]
struct InputMask {
    type_: u32,
    codes_size: u32,
    codes_ptr: u64,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct InputEvent {
    sec: i64,
    usec: i64,
    type_: u16,
    code: u16,
    value: i32,
}

const _: () = assert!(size_of::<InputEvent>() == 24);

unsafe fn ioctl_ptr<T>(fd: &File, req: u64, arg: *const T) -> bool {
    // SAFETY: callers pass a live descriptor and an argument that outlives
    // the call; the ioctl itself only reads from it.
    unsafe {
        libc::ioctl(
            fd.as_raw_fd(),
            req as libc::c_ulong,
            arg as *const libc::c_void,
        ) >= 0
    }
}

fn open_device(path: &Path) -> Option<File> {
    use std::os::unix::fs::OpenOptionsExt;
    let f = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
        .ok()?;
    let meta = f.metadata().ok()?;
    use std::os::unix::fs::FileTypeExt;
    if meta.file_type().is_char_device() {
        Some(f)
    } else {
        None
    }
}

/// Capability bitmap of `EV_KEY` codes the device can emit.
fn key_caps(fd: &File) -> Option<[u8; 96]> {
    let mut bits = [0u8; 96];
    let ok = unsafe { ioctl_ptr(fd, EVIOCGBIT_KEY, bits.as_mut_ptr()) };
    ok.then_some(bits)
}

fn has_code(bits: &[u8; 96], code: u16) -> bool {
    bits.get(code as usize / 8)
        .is_some_and(|b| (b >> (code % 8)) & 1 == 1)
}

fn list_mode() {
    println!("evdev keyboards in /dev/input/by-id (trigger candidates):\n");
    let mut found = false;
    for entry in std::fs::read_dir("/dev/input/by-id")
        .ok()
        .into_iter()
        .flatten()
        .flatten()
    {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.ends_with("-event-kbd") && !name.ends_with("event-kbd") {
            continue;
        }
        let path: PathBuf = entry.path();
        let Some(dev) = open_device(&path) else {
            continue;
        };
        let Some(caps) = key_caps(&dev) else {
            println!("  {} — EVIOCGBIT failed", name);
            continue;
        };
        found = true;
        let supported: Vec<&str> = KEY_NAMES
            .iter()
            .filter(|(code, _)| has_code(&caps, *code))
            .map(|(_, n)| *n)
            .collect();
        println!("  {}", name);
        println!("    candidates: {}", supported.join(", "));
    }
    if !found {
        println!("  (no keyboards found)");
    }
    println!("\nno elevated access needed — uaccess ACLs handle it.");
}

fn set_masks(dev: &File, trigger: u16) -> Result<(), String> {
    // EV_KEY: deliver only the trigger. Bitmap length must be a multiple of
    // sizeof(c_long), so round up.
    let need = (trigger as usize / 8) + 1;
    let align = size_of::<libc::c_long>();
    let len = need.div_ceil(align) * align;
    let mut bits = vec![0u8; len];
    bits[trigger as usize / 8] = 1 << (trigger % 8);
    let key_mask = InputMask {
        type_: EV_KEY as u32,
        codes_size: len as u32,
        codes_ptr: bits.as_ptr() as u64,
    };
    if !unsafe { ioctl_ptr(dev, EVIOCSMASK, &key_mask) } {
        return Err(format!(
            "EVIOCSMASK(EV_KEY): {}",
            std::io::Error::last_os_error()
        ));
    }

    // EV_MSC: zero-length mask clears the type entirely — no MSC_SCAN
    // scancodes for keys we are not watching.
    let msc_mask = InputMask {
        type_: EV_MSC as u32,
        codes_size: 0,
        codes_ptr: 0,
    };
    if !unsafe { ioctl_ptr(dev, EVIOCSMASK, &msc_mask) } {
        return Err(format!(
            "EVIOCSMASK(EV_MSC): {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

fn grab(dev: &File, on: bool) -> bool {
    // EVIOCGRAB does NOT dereference its argument: the argument value itself
    // selects grab (non-zero) or ungrab (zero). Pass a literal, never a
    // pointer — a pointer is always non-zero and therefore always grabs.
    let ok = unsafe {
        libc::ioctl(
            dev.as_raw_fd(),
            EVIOCGRAB as libc::c_ulong,
            on as libc::c_ulong as *const libc::c_void,
        ) >= 0
    };
    if !ok {
        let err = std::io::Error::last_os_error();
        println!("    EVIOCGRAB({on}) failed: {err}");
        let _ = std::io::stdout().flush();
    }
    ok
}

/// Capture mode (rebind-style, like cosmic-voice): every keyboard node open,
/// no key mask (MSC still suppressed). Prints raw key codes only. Used to
/// discover which device node a key actually emits on. Bounded by `secs`.
fn capture_mode(secs: u64) -> i32 {
    let mut devices: Vec<(File, String)> = Vec::new();
    for entry in std::fs::read_dir("/dev/input/by-id")
        .ok()
        .into_iter()
        .flatten()
        .flatten()
    {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.ends_with("event-kbd") {
            continue;
        }
        let Some(dev) = open_device(&entry.path()) else {
            continue;
        };
        let msc_mask = InputMask {
            type_: EV_MSC as u32,
            codes_size: 0,
            codes_ptr: 0,
        };
        if unsafe { ioctl_ptr(&dev, EVIOCSMASK, &msc_mask) } {
            devices.push((dev, name));
        }
    }
    if devices.is_empty() {
        eprintln!("no keyboards could be opened");
        return 4;
    }
    println!(
        "capturing on {} keyboard node(s) for {secs}s — press keys now",
        devices.len()
    );

    let mut poll_fds: Vec<libc::pollfd> = devices
        .iter()
        .map(|(f, _)| libc::pollfd {
            fd: f.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        })
        .collect();
    let deadline = Instant::now() + Duration::from_secs(secs);
    let mut seen = 0u32;

    while Instant::now() < deadline {
        let rc = unsafe { libc::poll(poll_fds.as_mut_ptr(), poll_fds.len() as libc::nfds_t, 250) };
        if rc <= 0 {
            continue;
        }
        for (i, (dev, name)) in devices.iter_mut().enumerate() {
            if poll_fds[i].revents & libc::POLLIN == 0 {
                continue;
            }
            let mut buf = [0u8; 24];
            while matches!(dev.read(&mut buf), Ok(n) if n == 24) {
                let ev = unsafe { std::ptr::read_unaligned(buf.as_ptr() as *const InputEvent) };
                if ev.type_ == EV_KEY && ev.value != 2 {
                    let key_name = KEY_NAMES
                        .iter()
                        .find(|(c, _)| *c == ev.code)
                        .map(|(_, n)| (*n).to_string())
                        .unwrap_or_else(|| format!("code {}", ev.code));
                    println!(
                        "  [{}] {:<14} ({}) value={}",
                        if name.contains("if02") {
                            "if02"
                        } else {
                            "main"
                        },
                        key_name,
                        ev.code,
                        if ev.value == 1 { "press" } else { "release" }
                    );
                    seen += 1;
                }
            }
        }
    }
    println!("captured {seen} key edges");
    0
}

fn watch_mode(device: Option<PathBuf>, code: u16, secs: u64) -> i32 {
    // Pick the first -event-kbd in by-id if unspecified.
    let device = device.unwrap_or_else(|| {
        std::fs::read_dir("/dev/input/by-id")
            .ok()
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .find(|p| {
                p.file_name()
                    .is_some_and(|n| n.to_string_lossy().ends_with("event-kbd"))
                    && !p.to_string_lossy().contains("if02")
            })
            .expect("no keyboard found in /dev/input/by-id")
    });

    let mut dev = match open_device(&device) {
        Some(d) => d,
        None => {
            eprintln!(
                "cannot open {} (permission denied? you must be on the active seat)",
                device.display()
            );
            return 4;
        }
    };
    println!("device: {}", device.display());

    if let Some(caps) = key_caps(&dev)
        && !has_code(&caps, code)
    {
        println!("WARNING: code {code} not in device capabilities — no events will arrive");
    }

    if let Err(e) = set_masks(&dev, code) {
        eprintln!("mask setup failed: {e}");
        return 4;
    }
    println!("mask: EV_KEY limited to code {code}, EV_MSC suppressed");

    println!(
        "holding watch for {secs}s — press and release the trigger a few times.\n\
         leak check: focus a text editor; while HOLDING the trigger, mash letter keys —\n\
         nothing should appear until you release.\n"
    );

    let deadline = Instant::now() + Duration::from_secs(secs);
    let start = Instant::now();
    let mut buf = [0u8; 24];

    let (mut presses, mut releases, mut repeats, mut syns) = (0u32, 0u32, 0u32, 0u32);
    let mut violations: Vec<String> = Vec::new();
    let mut grabbed = false;

    let mut poll_fd = libc::pollfd {
        fd: dev.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };

    while Instant::now() < deadline {
        unsafe {
            let rc = libc::poll(&mut poll_fd, 1, 250);
            if rc <= 0 {
                continue;
            }
        }
        // Drain everything currently queued; nonblocking fd, so a short read
        // ends the drain instead of blocking.
        loop {
            match dev.read(&mut buf) {
                Ok(24) => {}
                Ok(_) => break,
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(_) => break,
            }
            let ev = unsafe { std::ptr::read_unaligned(buf.as_ptr() as *const InputEvent) };
            match (ev.type_, ev.code, ev.value) {
                (EV_SYN, _, _) => syns += 1,
                (EV_KEY, c, v) if c == code => {
                    let t = start.elapsed();
                    match v {
                        1 => {
                            presses += 1;
                            if !grabbed {
                                grabbed = grab(&dev, true);
                                println!(
                                    "[{:>7.3}s] PRESS   — EVIOCGRAB {}",
                                    t.as_secs_f64(),
                                    if grabbed { "acquired" } else { "FAILED" }
                                );
                            } else {
                                println!("[{:>7.3}s] PRESS   (already grabbed)", t.as_secs_f64());
                            }
                            let _ = std::io::stdout().flush();
                        }
                        0 => {
                            releases += 1;
                            if grabbed {
                                grab(&dev, false);
                                grabbed = false;
                            }
                            println!("[{:>7.3}s] RELEASE — grab released", t.as_secs_f64());
                            let _ = std::io::stdout().flush();
                        }
                        2 => repeats += 1,
                        _ => {}
                    }
                }
                _ => violations.push(format!(
                    "type={} code={} value={} delivered through the mask!",
                    ev.type_, ev.code, ev.value
                )),
            }
        }
    }

    if grabbed {
        grab(&dev, false);
    }

    println!(
        "\nsummary: {presses} presses, {releases} releases, {repeats} autorepeats, {syns} SYN"
    );
    println!(
        "non-trigger events delivered: {} (expect 0)",
        violations.len()
    );
    for v in &violations {
        println!("  VIOLATION: {v}");
    }

    if !violations.is_empty() {
        return 2;
    }
    if presses == 0 {
        println!("no trigger presses seen — wrong device, wrong code, or you didn't press");
        return 3;
    }
    if releases == 0 {
        println!("presses but no release seen — hold-to-talk edge incomplete");
        return 3;
    }
    println!("PASS: press+release edges arrive, nothing else delivered");
    0
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let get = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let secs: u64 = get("--secs").and_then(|s| s.parse().ok()).unwrap_or(20);
    let code: u16 = get("--code")
        .and_then(|s| s.parse().ok())
        .unwrap_or(KEY_F13);

    if args.iter().any(|a| a == "--list") {
        list_mode();
        return;
    }
    if args.iter().any(|a| a == "--capture") {
        std::process::exit(capture_mode(secs));
    }
    std::process::exit(watch_mode(get("--device").map(PathBuf::from), code, secs));
}
