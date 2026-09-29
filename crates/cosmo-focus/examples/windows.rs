//! What the window verbs see (read-only): every window with its app id,
//! focus and workspaces; every workspace with its name, position and
//! state. `--noop-switch` also exercises the request path harmlessly, by
//! switching to the workspace that is already active.
//!
//! `cargo run -p cosmo-focus --example windows [--noop-switch]`

use std::time::Instant;

use cosmo_focus::control::{WindowService, find_workspace};

fn main() -> anyhow::Result<()> {
    let t = Instant::now();
    let control = WindowService::start()?;
    let connected = t.elapsed();
    let t = Instant::now();
    let snap = control.snapshot()?;
    let asked = t.elapsed();
    for w in &snap.workspaces {
        println!(
            "workspace {:>3} at {:?}{} (group {:?})",
            w.name,
            w.coordinates,
            if w.active { " ACTIVE" } else { "" },
            w.group
        );
    }
    for w in &snap.windows {
        println!(
            "{} {:<28} workspaces {:?}",
            if w.activated { "*" } else { " " },
            w.app_id,
            w.workspaces
        );
    }
    println!(
        "service up in {:.1} ms; a snapshot from it in {:.2} ms",
        connected.as_secs_f64() * 1e3,
        asked.as_secs_f64() * 1e3
    );

    if std::env::args().any(|a| a == "--noop-switch") {
        let active = snap.workspaces.iter().position(|w| w.active);
        let n = active
            .and_then(|a| (1..=20).find(|&n| find_workspace(&snap, n) == Some(a)))
            .expect("the active workspace has a number");
        let t = Instant::now();
        control.switch_workspace(n)?;
        println!(
            "switched to workspace {n} (already active) in {:.1} ms",
            t.elapsed().as_secs_f64() * 1e3
        );
    }
    Ok(())
}
