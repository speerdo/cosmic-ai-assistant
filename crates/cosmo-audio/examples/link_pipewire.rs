//! Link spike (spec §2.1): prove the `pipewire` crate resolves, generates
//! its bindings against this machine's libpipewire, and links — then that
//! the symbols are really there by connecting to the running session.
//! It plays nothing; §2.3 owns the real playback stream.
//!
//! `cargo run -p cosmo-audio --example link_pipewire --features pipewire-backend`
//!
//! Note for §2.3: pipewire 0.10 replaced 0.8's plain `MainLoop::new()` with
//! explicit ownership variants (`MainLoopRc` / `MainLoopBox`), so the
//! cosmic-voice code the plan calls "close to liftable" needs this rename
//! before it compiles here.

use pipewire as pw;

fn main() -> Result<(), pw::Error> {
    pw::init();
    let main_loop = pw::main_loop::MainLoopRc::new(None)?;
    let context = pw::context::ContextRc::new(&main_loop, None)?;
    let core = context.connect_rc(None)?;
    println!("pipewire linked; session core connected");
    drop(core);
    Ok(())
}
