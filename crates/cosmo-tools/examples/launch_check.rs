//! How `launch_app` would start each installed application, without
//! starting anything: the argv from every `Exec=` line, or why it's refused.
//!
//! `cargo run -p cosmo-tools --example launch_check`

fn main() {
    let (mut ok, mut refused) = (0, 0);
    for app in cosmo_stt::hotwords::desktop_apps() {
        let result = match (&app.exec, app.terminal) {
            (_, true) => Err("runs in a terminal".to_owned()),
            (None, _) => Err("no Exec".to_owned()),
            (Some(exec), _) => cosmo_tools::launch::split_exec(exec, &app.name),
        };
        match result {
            Ok(argv) => {
                ok += 1;
                println!("  {:<44} {argv:?}", app.id);
            }
            Err(e) => {
                refused += 1;
                println!("✗ {:<44} {e}", app.id);
            }
        }
    }
    println!("{ok} launchable, {refused} refused");
}
