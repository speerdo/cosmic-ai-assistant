//! The news tool against every suggested feed, live: how many headlines
//! each yields, and the newest. `cargo run -p cosmo-tools --example news`

#[tokio::main(flavor = "current_thread")]
async fn main() {
    for (key, name, url) in cosmo_tools::news::SUGGESTED {
        let feed = cosmo_config::profile::Feed {
            name: (*name).into(),
            url: (*url).into(),
        };
        match cosmo_tools::news::headlines(&[feed], None, None, 2, chrono::Utc::now()).await {
            Ok(text) => {
                let lines: Vec<&str> = text.lines().skip(1).collect();
                println!(
                    "{key:<10} {}",
                    lines.join(" | ").chars().take(220).collect::<String>()
                );
            }
            Err(e) => println!("{key:<10} ERROR {e}"),
        }
    }
}
