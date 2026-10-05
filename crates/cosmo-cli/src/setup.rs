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

/// One answer from the terminal; Enter keeps `default`.
pub fn ask(question: &str, default: &str) -> String {
    use std::io::Write;
    if default.is_empty() {
        print!("{question}: ");
    } else {
        print!("{question} [{default}]: ");
    }
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
    let line = line.trim();
    if line.is_empty() {
        default.to_owned()
    } else {
        line.to_owned()
    }
}

/// `cosmo setup`, part one: the profile (name, home, units). Saved to
/// `~/.config/cosmo/profile.json`, owner-only.
pub async fn setup_profile() -> Result<(), String> {
    use cosmo_config::profile::{self, Units};
    let mut p = profile::load()?;
    println!("About you (Enter keeps what's in brackets; nothing here is required).\n");

    let name = ask(
        "What should cosmo call you",
        p.name.as_deref().unwrap_or(""),
    );
    p.name = (!name.is_empty()).then_some(name);

    let current = p.home.as_ref().map(|h| h.name.clone()).unwrap_or_default();
    loop {
        let town = ask("Your town or city, for the weather", &current);
        if town.is_empty() || town == current {
            break;
        }
        match cosmo_tools::geo::geocode(&town).await {
            Ok(found) if found.is_empty() => println!("  Nothing called {town:?} was found."),
            Ok(found) => {
                for (i, place) in found.iter().enumerate() {
                    println!("  {}) {}", i + 1, place.name);
                }
                let pick = ask("  Which one (or 0 to type it again)", "1");
                match pick.parse::<usize>() {
                    Ok(n) if (1..=found.len()).contains(&n) => {
                        p.home = Some(found[n - 1].clone());
                        break;
                    }
                    _ => continue,
                }
            }
            Err(e) => {
                println!("  Couldn't look it up ({e}); try again, or press Enter to skip.");
            }
        }
    }

    let default = match (p.home.is_some() || p.name.is_some(), p.units) {
        (false, _) => Units::from_locale(),
        (true, u) => u,
    };
    let units = ask(
        "Units, metric or imperial",
        match default {
            Units::Metric => "metric",
            Units::Imperial => "imperial",
        },
    );
    p.units = if units.to_lowercase().starts_with('i') {
        Units::Imperial
    } else {
        Units::Metric
    };

    let path = profile::save(&p)?;
    println!(
        "\nSaved to {} (only you can read it). The reasoning model is told your name and \
         town; your coordinates go only to the weather service.\n{}\n{}",
        path.display(),
        cosmo_tools::geo::ATTRIBUTION,
        cosmo_tools::weather::ATTRIBUTION
    );
    Ok(())
}

/// `cosmo profile`: what's saved.
pub fn show_profile() -> i32 {
    match cosmo_config::profile::load() {
        Ok(p) => {
            println!(
                "profile: {}",
                cosmo_config::profile::profile_path().display()
            );
            println!("  name:  {}", p.name.as_deref().unwrap_or("(not set)"));
            println!(
                "  home:  {}",
                p.home.as_ref().map_or("(not set)", |h| h.name.as_str())
            );
            println!("  units: {:?}", p.units);
            if p.home.is_none() {
                println!("set it with: cosmo setup");
            }
            0
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

/// Ask a yes/no; Enter takes `default`.
pub fn yes(question: &str, default: bool) -> bool {
    // The hint is shown, never taken as the answer.
    let answer = ask(
        &format!("{question} [{}]", if default { "Y/n" } else { "y/N" }),
        "",
    );
    match answer.to_lowercase().chars().next() {
        Some('y') => true,
        Some('n') => false,
        _ => default,
    }
}
