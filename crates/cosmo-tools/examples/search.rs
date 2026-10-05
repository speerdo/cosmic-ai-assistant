//! web_search and read_page against the live services.
//!
//! `cargo run -p cosmo-tools --example search -- "Dune 3 release date"`
//! `cargo run -p cosmo-tools --example search -- --page https://en.wikipedia.org/wiki/Stonehenge`

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let out = match args.first().map(String::as_str) {
        Some("--page") => cosmo_tools::search::read_page(&args[1]).await,
        _ => {
            let q = args.join(" ");
            cosmo_tools::search::web_search("wikipedia", None, &q, 5, &cosmo_tools::search::today())
                .await
        }
    };
    match out {
        Ok(text) => println!("{text}"),
        Err(e) => println!("ERROR {e}"),
    }
}
