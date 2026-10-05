//! The weather tool against the live services: geocode a place, then
//! print the forecast summary the model would get.
//!
//! `cargo run -p cosmo-tools --example weather -- Oslo [imperial]`

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let place = args.next().unwrap_or_else(|| "Oslo".into());
    let units = match args.next().as_deref() {
        Some("imperial") => cosmo_config::profile::Units::Imperial,
        _ => cosmo_config::profile::Units::Metric,
    };
    let found = cosmo_tools::geo::geocode(&place).await?;
    for p in &found {
        println!("match: {} ({:.4}, {:.4})", p.name, p.latitude, p.longitude);
    }
    let home = found.first().ok_or("nothing found")?;
    let t = std::time::Instant::now();
    println!("\n{}", cosmo_tools::weather::forecast(home, units).await?);
    println!("\n(first fetch {} ms)", t.elapsed().as_millis());
    let t = std::time::Instant::now();
    cosmo_tools::weather::forecast(home, units).await?;
    println!("(second, cached: {} ms)", t.elapsed().as_millis());
    Ok(())
}
