//! What `cosmo` checks and does on this machine without the daemon (phase-8
//! §8.2, §8.4): the models, the first-run fetch, and the half of `doctor`
//! that must work when the daemon isn't running.

use std::path::{Path, PathBuf};
use std::process::Command;

use cosmo_ipc::DoctorCheck;

/// The fetcher, wherever this install put it: `$COSMO_FETCH_MODELS`, beside
/// the binary (`../libexec/cosmo/`, for `/usr` and `~/.local` alike),
/// `/usr/libexec/cosmo/`, or a checkout's `scripts/`.
pub fn fetcher() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("COSMO_FETCH_MODELS").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(p));
    }
    let exe = std::env::current_exe().ok()?;
    let bin = exe.parent()?;
    let mut candidates = vec![
        bin.join("../libexec/cosmo/fetch-models"),
        PathBuf::from("/usr/libexec/cosmo/fetch-models"),
    ];
    // target/{debug,release}/cosmo in a checkout.
    candidates.extend(
        bin.ancestors()
            .nth(2)
            .map(|r| r.join("scripts/fetch-models")),
    );
    candidates.into_iter().find(|p| p.is_file())
}

/// `cosmo models`: the default set, present or not.
pub fn models_status() -> i32 {
    let Some(root) = cosmo_config::models::root() else {
        eprintln!("neither XDG_CACHE_HOME nor HOME is set");
        return 1;
    };
    let set = cosmo_config::models::default_set(&root);
    println!("models in {}:", root.display());
    for m in &set {
        let mark = if m.present { "✓" } else { "✗" };
        let size = m
            .present
            .then(|| dir_size(m.path.parent().unwrap_or(&m.path)))
            .map(|b| format!("{:>5} MB", b / 1_000_000))
            .unwrap_or_else(|| "missing ".into());
        println!("  {mark} {:<24} {size}  {}", m.name, m.role);
    }
    if set.iter().all(|m| m.present) {
        0
    } else {
        println!("fetch the missing ones with: cosmo models fetch");
        1
    }
}

fn dir_size(p: &Path) -> u64 {
    match std::fs::metadata(p) {
        Ok(m) if m.is_dir() => std::fs::read_dir(p)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| dir_size(&e.path()))
            .sum(),
        Ok(m) => m.len(),
        Err(_) => 0,
    }
}

/// `cosmo models fetch [args…]`: the default set (Kokoro, VAD, both ASR
/// models), checksummed; `args` go to the fetcher as they are (e.g.
/// `--variant q8`). Then a running daemon is restarted to load them.
pub fn models_fetch(args: &[String]) -> i32 {
    let Some(fetcher) = fetcher() else {
        eprintln!(
            "fetch-models not found (looked beside this binary, in \
             /usr/libexec/cosmo, and in a checkout's scripts/); set \
             COSMO_FETCH_MODELS to its path"
        );
        return 1;
    };
    let mut cmd = Command::new(&fetcher);
    cmd.args(["--kokoro", "--vad", "--asr"]).args(args);
    match cmd.status() {
        Ok(s) if s.success() => {}
        Ok(s) => {
            eprintln!("fetch failed ({s}); re-run to resume — verified files are kept");
            return 1;
        }
        Err(e) => {
            eprintln!("could not run {}: {e}", fetcher.display());
            return 1;
        }
    }
    // try-restart: only if it's running under systemd already.
    let restarted = Command::new("systemctl")
        .args(["--user", "try-restart", "cosmo.service"])
        .status()
        .is_ok_and(|s| s.success());
    if restarted {
        println!("models in place; restarted the daemon (cosmo.service) to load them");
    } else {
        println!("models in place; restart the daemon to load them");
    }
    0
}

/// The local half of `cosmo doctor`.
pub fn local_checks() -> Vec<DoctorCheck> {
    let cfg = cosmo_config::load().unwrap_or_default();
    let mut checks = vec![service_check(), models_check()];
    if cfg.voice_provider == "kokoro" {
        checks.push(espeak_check());
    }
    checks.push(agent_check(&cfg.agent_command));
    checks.push(display_check());
    checks
}

fn check(name: &str, ok: bool, detail: impl Into<String>) -> DoctorCheck {
    DoctorCheck {
        name: name.into(),
        ok,
        warn: false,
        detail: detail.into(),
    }
}

fn systemctl_user(verb: &str) -> Option<String> {
    let out = Command::new("systemctl")
        .args(["--user", verb, "cosmo.service"])
        .output()
        .ok()?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

fn service_check() -> DoctorCheck {
    let (Some(enabled), Some(active)) = (systemctl_user("is-enabled"), systemctl_user("is-active"))
    else {
        return check(
            "service",
            false,
            "systemctl not found — run cosmod yourself",
        );
    };
    match (enabled.as_str(), active.as_str()) {
        ("enabled", "active") => check("service", true, "cosmo.service enabled and running"),
        (_, "active") => check(
            "service",
            true,
            format!("cosmo.service running but {enabled}: `systemctl --user enable cosmo`"),
        ),
        ("" | "not-found", _) => check(
            "service",
            false,
            "cosmo.service isn't installed (install the package, or scripts/install-dev)",
        ),
        _ => check(
            "service",
            false,
            format!(
                "cosmo.service is {enabled}, {active}: `systemctl --user enable --now cosmo`; \
                 `journalctl --user -u cosmo` says why it stopped"
            ),
        ),
    }
}

fn models_check() -> DoctorCheck {
    let Some(root) = cosmo_config::models::root() else {
        return check("models", false, "neither XDG_CACHE_HOME nor HOME is set");
    };
    let set = cosmo_config::models::default_set(&root);
    let missing: Vec<_> = set.iter().filter(|m| !m.present).map(|m| m.name).collect();
    if missing.is_empty() {
        check(
            "models",
            true,
            format!("all {} in {}", set.len(), root.display()),
        )
    } else {
        check(
            "models",
            false,
            format!("missing {}: run `cosmo models fetch`", missing.join(", ")),
        )
    }
}

/// Kokoro's phonemizer, loaded at run time exactly as Kokoro loads it.
#[allow(unsafe_code)]
fn espeak_check() -> DoctorCheck {
    const LIB: &std::ffi::CStr = c"libespeak-ng.so.1";
    // SAFETY: dlopen/dlclose on a NUL-terminated name; the handle is closed
    // before return and nothing from the library is called.
    let found = unsafe {
        let h = libc::dlopen(LIB.as_ptr(), libc::RTLD_LAZY | libc::RTLD_LOCAL);
        if !h.is_null() {
            libc::dlclose(h);
        }
        !h.is_null()
    };
    if found {
        check(
            "espeak-ng",
            true,
            "libespeak-ng.so.1 loads (Kokoro's phonemes)",
        )
    } else {
        check(
            "espeak-ng",
            false,
            "libespeak-ng.so.1 not found — Kokoro can't speak: install espeak-ng \
             (`libespeak-ng1` on Pop!_OS, `espeak-ng` on Fedora)",
        )
    }
}

fn agent_check(command: &str) -> DoctorCheck {
    match cosmo_config::locate::agent(command) {
        Some(found) => check("agent install", true, found.program.display().to_string()),
        None => check(
            "agent install",
            false,
            format!(
                "`{command}` not found — `npm install -g @agent-sh/computer-use-linux` \
                 (cosmo works without it, but can't drive windows)"
            ),
        ),
    }
}

/// Layer shell, asked of the overlay itself (it's the one that needs it).
fn display_check() -> DoctorCheck {
    let overlay = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.join("cosmo-overlay")))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from("cosmo-overlay"));
    match Command::new(&overlay).arg("--check-layer-shell").status() {
        Ok(s) if s.success() => check("overlay", true, "layer shell available: the overlay shows"),
        Ok(s) if s.code() == Some(1) => check(
            "overlay",
            true,
            "no layer shell here (GNOME?): the overlay falls back to notifications",
        ),
        Ok(_) => check(
            "overlay",
            false,
            "no Wayland display to check (run from the session)",
        ),
        Err(_) => check("overlay", false, "cosmo-overlay not installed"),
    }
}
