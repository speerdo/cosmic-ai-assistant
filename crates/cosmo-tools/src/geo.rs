//! Place names to coordinates, through OpenStreetMap's Nominatim: once at
//! setup ("Pittsburgh"), or when the user names another place for the
//! weather. Its usage policy: an identifying User-Agent, at most one
//! request a second, results cached, attribution (© OpenStreetMap
//! contributors, ODbL). No autocomplete, no bulk lookups.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use cosmo_config::profile::Place;

/// Who's asking, as the policy wants: the app and where to find it.
pub fn user_agent() -> String {
    format!(
        "cosmo/{} (+https://github.com/speerdo/cosmic-ai-assistant)",
        env!("CARGO_PKG_VERSION")
    )
}

/// Attribution, for wherever a looked-up place is shown.
pub const ATTRIBUTION: &str = "Places: © OpenStreetMap contributors (ODbL), via Nominatim";

static LAST: Mutex<Option<Instant>> = Mutex::new(None);
static CACHE: Mutex<Option<HashMap<String, Vec<Place>>>> = Mutex::new(None);

/// Up to three places matching `query`, best first.
pub async fn geocode(query: &str) -> Result<Vec<Place>, String> {
    let key = query.trim().to_lowercase();
    if key.is_empty() {
        return Err("no place given".into());
    }
    if let Some(hit) = CACHE
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|c| c.get(&key).cloned())
    {
        return Ok(hit);
    }
    // One request a second, across the whole process.
    let wait = {
        let mut last = LAST.lock().unwrap();
        let wait = last
            .map(|t| Duration::from_millis(1100).saturating_sub(t.elapsed()))
            .unwrap_or_default();
        *last = Some(Instant::now() + wait);
        wait
    };
    tokio::time::sleep(wait).await;
    let resp = reqwest::Client::new()
        .get("https://nominatim.openstreetmap.org/search")
        .query(&[
            ("q", query.trim()),
            ("format", "jsonv2"),
            ("limit", "3"),
            ("addressdetails", "1"),
            ("accept-language", "en"),
        ])
        .header(reqwest::header::USER_AGENT, user_agent())
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| format!("looking up {query:?}: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("looking up {query:?}: {}", resp.status()));
    }
    let body: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    let found = parse(&body);
    CACHE
        .lock()
        .unwrap()
        .get_or_insert_with(HashMap::new)
        .insert(key, found.clone());
    Ok(found)
}

/// Nominatim's results as places with short names ("Pittsburgh,
/// Pennsylvania, United States", not the county and postcode too).
fn parse(body: &serde_json::Value) -> Vec<Place> {
    let mut places: Vec<Place> = Vec::new();
    // A city and its municipality share a name: one choice, not two.
    for p in parse_all(body) {
        if !places.iter().any(|q| q.name == p.name) {
            places.push(p);
        }
    }
    places
}

fn parse_all(body: &serde_json::Value) -> Vec<Place> {
    body.as_array()
        .into_iter()
        .flatten()
        .filter_map(|r| {
            let lat = r["lat"].as_str()?.parse().ok()?;
            let lon = r["lon"].as_str()?.parse().ok()?;
            let a = &r["address"];
            let local = [
                "city",
                "town",
                "village",
                "hamlet",
                "municipality",
                "county",
            ]
            .iter()
            .find_map(|k| a[*k].as_str())
            .or_else(|| r["name"].as_str());
            let parts: Vec<&str> = [local, a["state"].as_str(), a["country"].as_str()]
                .into_iter()
                .flatten()
                .collect();
            let name = if parts.is_empty() {
                r["display_name"].as_str()?.to_owned()
            } else {
                parts.join(", ")
            };
            Some(Place {
                name,
                latitude: lat,
                longitude: lon,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nominatim_results_get_short_names() {
        let body = serde_json::json!([
            {"lat": "40.4416941", "lon": "-79.9900861", "name": "Pittsburgh",
             "display_name": "Pittsburgh, Allegheny County, Pennsylvania, United States",
             "address": {"city": "Pittsburgh", "county": "Allegheny County",
                         "state": "Pennsylvania", "country": "United States"}},
            {"lat": "bad", "lon": "0"},
            {"lat": "51.5", "lon": "-0.12", "display_name": "London, England"}
        ]);
        let places = parse(&body);
        assert_eq!(places.len(), 2);
        assert_eq!(places[0].name, "Pittsburgh, Pennsylvania, United States");
        assert!((places[0].latitude - 40.4416941).abs() < 1e-9);
        assert_eq!(places[1].name, "London, England");
        let twice = serde_json::json!([
            {"lat": "59.91", "lon": "10.73", "address": {"city": "Oslo", "country": "Norway"}},
            {"lat": "59.97", "lon": "10.77", "address": {"city": "Oslo", "country": "Norway"}}
        ]);
        assert_eq!(parse(&twice).len(), 1, "same name: one choice");
    }
}
