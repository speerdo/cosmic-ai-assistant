//! Link spike (spec §2.1): prove `ort` — the ONNX runtime §2.5's Kokoro
//! provider will sit on — resolves, links, and reaches a live runtime.
//! Loads no model and speaks nothing; §2.5 owns the real provider.
//!
//! `cargo run -p cosmo-tts --example link_ort --features kokoro`

fn main() {
    // Committing the environment is the call that forces the runtime to be
    // located: a build that compiles but cannot find libonnxruntime fails
    // here, which is the distinction the spike exists to draw. `commit()`
    // returns false if an environment was already committed, not on error.
    let committed = ort::init().with_name("cosmo-link-spike").commit();
    println!("ort environment committed: {committed}");
}
