//! Try the reflex path on text, with the real matcher, gate check and
//! desktop actuator. **Dry run by default**: prints what each line would
//! do. `--act` carries the actions out.
//!
//! `cargo run -p cosmo-daemon --example reflex_try -- [--act] "open firefox" …`
//! `… -- --list scripts/bench-commands.txt`   (every line of a file, dry)

use std::time::Instant;

use cosmo_daemon::reflex::Reflex;
use cosmo_reflex::THRESHOLD;

#[tokio::main]
async fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let act = args.first().is_some_and(|a| a == "--act");
    if act {
        args.remove(0);
    }
    if args.first().is_some_and(|a| a == "--list") {
        let text = std::fs::read_to_string(&args[1]).expect("list file");
        args = text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .map(String::from)
            .collect();
    }
    let t = Instant::now();
    let reflex = tokio::task::spawn_blocking(Reflex::desktop).await.unwrap();
    println!(
        "reflex ready in {:.0} ms\n",
        t.elapsed().as_secs_f64() * 1e3
    );
    for text in &args {
        let t = Instant::now();
        let matched = reflex.matcher.match_text(text);
        let matched_in = t.elapsed();
        match matched {
            Some(m) if m.confidence >= THRESHOLD => {
                let (tool, args) = m.intent.tool_call();
                print!(
                    "{text:<72} → {} ({:.2}) [{tool} {args}] in {:.2} ms",
                    m.intent.describe(),
                    m.confidence,
                    matched_in.as_secs_f64() * 1e3
                );
                if act {
                    let t = Instant::now();
                    let r = reflex.actuator.act(&m.intent).await;
                    print!("  ⇒ {r:?} in {:.1} ms", t.elapsed().as_secs_f64() * 1e3);
                }
                println!();
            }
            Some(m) => println!(
                "{text:<72} → escalate ({} at {:.2}, under {THRESHOLD})",
                m.intent.describe(),
                m.confidence
            ),
            None => println!("{text:<72} → escalate (no reflex verb)"),
        }
    }
}
