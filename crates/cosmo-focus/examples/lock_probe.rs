//! Records what a normal Wayland client can see while COSMIC locks and
//! unlocks (phase-8 lock investigation): every change in which window is
//! activated, which workspace is active, and how many windows exist,
//! timestamped. Run it, lock the screen, unlock, then Ctrl+C.
//!
//! `cargo run -p cosmo-focus --example lock_probe`

use std::time::{Duration, Instant};

use cosmo_focus::control::WindowService;

fn main() -> anyhow::Result<()> {
    let control = WindowService::start()?;
    let t0 = Instant::now();
    let mut last = String::new();
    loop {
        let snap = control.snapshot()?;
        let focused = snap
            .focused()
            .map(|i| snap.windows[i].app_id.clone())
            .unwrap_or_else(|| "NONE".into());
        let active: Vec<&str> = snap
            .workspaces
            .iter()
            .filter(|w| w.active)
            .map(|w| w.name.as_str())
            .collect();
        let line = format!(
            "activated={focused} windows={} active_workspaces={active:?}",
            snap.windows.len()
        );
        if line != last {
            let _ = t0;
            let now = std::process::Command::new("date")
                .arg("+%T.%3N")
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
                .unwrap_or_default();
            println!("{now} {line}");
            last = line;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}
