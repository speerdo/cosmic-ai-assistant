//! Print what the focus mirror sees: every toplevel, its workspaces, and
//! which one is focused.
//!
//! `cargo run -p cosmo-focus --example focus`

fn main() -> anyhow::Result<()> {
    let mirror = cosmo_focus::FocusMirror::connect()?;
    for t in mirror.snapshot().toplevels {
        println!(
            "{} {:<32} {:?} {:?}",
            if t.activated { "*" } else { " " },
            t.app_id.as_deref().unwrap_or("?"),
            t.title.as_deref().unwrap_or(""),
            t.workspaces
        );
    }
    println!("focused: {:?}", mirror.focused_app_id());
    Ok(())
}
