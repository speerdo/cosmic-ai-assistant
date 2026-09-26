//! Watch the trigger and print each edge (phase-3 spec §3.4).
//!
//! `cargo run -p cosmo-hotkey --example trigger -- [secs] [keycode]`
//! Defaults: 20 s, Right Ctrl (97). Hold the key, release it, repeat; try
//! unplugging and replugging a keyboard mid-run.

use std::time::{Duration, Instant};

use cosmo_hotkey::{Edge, KEY_RIGHTCTRL, Watcher};

fn main() {
    let mut args = std::env::args().skip(1);
    let secs: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(20);
    let code: u16 = args
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(KEY_RIGHTCTRL);
    let t0 = Instant::now();
    let pressed_at = std::sync::Mutex::new(None::<Instant>);
    let watcher = Watcher::start(code, move |ev| {
        let t = ev.at.duration_since(t0).as_secs_f64();
        match ev.edge {
            Edge::Pressed => {
                *pressed_at.lock().unwrap() = Some(ev.at);
                println!("[{t:7.3}s] PRESS");
            }
            Edge::Released => {
                let held = pressed_at
                    .lock()
                    .unwrap()
                    .take()
                    .map(|p| ev.at.duration_since(p));
                let note = match held {
                    Some(h) if h < Duration::from_millis(300) => {
                        " (under 300 ms: a tap/shortcut, would be ignored)"
                    }
                    _ => "",
                };
                println!(
                    "[{t:7.3}s] RELEASE after {:.0} ms{note}",
                    held.unwrap_or_default().as_secs_f64() * 1e3
                );
            }
        }
    })
    .expect("inotify on /dev/input");
    std::thread::sleep(Duration::from_millis(300));
    println!("watching keycode {code} on: {:#?}", watcher.devices());
    println!(
        "for {secs}s — only this key is ever delivered to this process (EVIOCSMASK); no grab."
    );
    std::thread::sleep(Duration::from_secs(secs));
}
