//! `weather`: the forecast for the user's home (from the profile) or a
//! named place, from MET Norway's Locationforecast (free, no key, CC BY
//! 4.0). Its terms, followed here: an identifying User-Agent, coordinates
//! to at most 4 decimals, and the response's `Expires` respected (one
//! cached forecast per place; `If-Modified-Since` when it's stale).
//!
//! The model gets a short text summary: now, the rest of today, and the
//! next three days, in the user's units. It phrases the answer.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Local, NaiveDate, Utc};
use cosmo_config::profile::{Place, Units};

pub const ATTRIBUTION: &str = "Weather: MET Norway (api.met.no), CC BY 4.0";

const URL: &str = "https://api.met.no/weatherapi/locationforecast/2.0/compact";

struct Cached {
    expires: SystemTime,
    last_modified: Option<String>,
    body: serde_json::Value,
}

/// Forecasts by rounded coordinates.
static CACHE: Mutex<Option<HashMap<(i64, i64), Cached>>> = Mutex::new(None);

/// Truncated to 4 decimals, as MET Norway requires.
fn coord(x: f64) -> f64 {
    (x * 10_000.0).trunc() / 10_000.0
}

/// The forecast summary for `place`.
pub async fn forecast(place: &Place, units: Units) -> Result<String, String> {
    let body = fetch(coord(place.latitude), coord(place.longitude)).await?;
    Ok(format!(
        "Forecast for {}:\n{}\n({ATTRIBUTION})",
        place.name,
        summarise(&body, units, Local::now())
    ))
}

async fn fetch(lat: f64, lon: f64) -> Result<serde_json::Value, String> {
    let key = ((lat * 10_000.0) as i64, (lon * 10_000.0) as i64);
    let stale_modified = {
        let cache = CACHE.lock().unwrap();
        match cache.as_ref().and_then(|c| c.get(&key)) {
            Some(c) if c.expires > SystemTime::now() => return Ok(c.body.clone()),
            Some(c) => c.last_modified.clone(),
            None => None,
        }
    };
    let mut req = reqwest::Client::new()
        .get(URL)
        .query(&[("lat", format!("{lat:.4}")), ("lon", format!("{lon:.4}"))])
        .header(reqwest::header::USER_AGENT, crate::geo::user_agent())
        .timeout(Duration::from_secs(10));
    if let Some(lm) = &stale_modified {
        req = req.header(reqwest::header::IF_MODIFIED_SINCE, lm);
    }
    let resp = req
        .send()
        .await
        .map_err(|e| format!("the weather service didn't answer: {e}"))?;
    let header = |name: reqwest::header::HeaderName| {
        resp.headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    };
    let expires = header(reqwest::header::EXPIRES)
        .and_then(|e| DateTime::parse_from_rfc2822(&e).ok())
        .map(|t| SystemTime::from(t.with_timezone(&Utc)))
        .unwrap_or_else(|| SystemTime::now() + Duration::from_secs(600));
    let last_modified = header(reqwest::header::LAST_MODIFIED);
    let status = resp.status();
    // The lock is held only here, never across an await.
    if status == reqwest::StatusCode::NOT_MODIFIED {
        let mut guard = CACHE.lock().unwrap();
        if let Some(c) = guard.get_or_insert_with(HashMap::new).get_mut(&key) {
            c.expires = expires;
            return Ok(c.body.clone());
        }
    }
    if !status.is_success() {
        return Err(format!("the weather service said {status}"));
    }
    let body: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    CACHE
        .lock()
        .unwrap()
        .get_or_insert_with(HashMap::new)
        .insert(
            key,
            Cached {
                expires,
                last_modified,
                body: body.clone(),
            },
        );
    Ok(body)
}

/// One day's figures, collected from the timeseries.
#[derive(Default)]
struct Day {
    high: Option<f64>,
    low: Option<f64>,
    rain_mm: f64,
    wind_max: f64,
    symbols: Vec<String>,
}

/// The forecast as a few lines of text, in `units`, with days by the
/// user's local date. Pure, so it's tested on a fixture.
pub fn summarise(body: &serde_json::Value, units: Units, now: DateTime<Local>) -> String {
    let series = body["properties"]["timeseries"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut days: Vec<(NaiveDate, Day)> = Vec::new();
    let mut current = None;
    // The series is hourly for ~2.5 days, then 6-hourly: count each
    // stretch of time's rain once.
    let mut covered_until: Option<DateTime<Utc>> = None;
    for entry in &series {
        let Some(t) = entry["time"]
            .as_str()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|t| t.with_timezone(&Utc))
        else {
            continue;
        };
        // Skip what's already past (but keep the current hour).
        if t + chrono::Duration::hours(1) <= now.with_timezone(&Utc) {
            continue;
        }
        let data = &entry["data"];
        let details = &data["instant"]["details"];
        let temp = details["air_temperature"].as_f64();
        let wind = details["wind_speed"].as_f64().unwrap_or(0.0);
        let (symbol, rain, span) = [
            ("next_1_hours", 1),
            ("next_6_hours", 6),
            ("next_12_hours", 12),
        ]
        .iter()
        .find_map(|(k, h)| {
            let block = &data[*k];
            block["summary"]["symbol_code"].as_str().map(|s| {
                (
                    s.to_owned(),
                    block["details"]["precipitation_amount"].as_f64(),
                    *h,
                )
            })
        })
        .map_or((None, None, 0), |(s, r, h)| (Some(s), r, h));
        if current.is_none() {
            current = Some((temp, wind, symbol.clone()));
        }
        let date = t.with_timezone(&Local).date_naive();
        if days.last().is_none_or(|(d, _)| *d != date) {
            if days.len() == 4 {
                break;
            }
            days.push((date, Day::default()));
        }
        let day = &mut days.last_mut().expect("just pushed").1;
        if let Some(c) = temp {
            day.high = Some(day.high.map_or(c, |h: f64| h.max(c)));
            day.low = Some(day.low.map_or(c, |l: f64| l.min(c)));
        }
        day.wind_max = day.wind_max.max(wind);
        if let Some(s) = symbol {
            day.symbols.push(plain(&s));
        }
        if let Some(r) = rain
            && covered_until.is_none_or(|c| t >= c)
        {
            day.rain_mm += r;
            covered_until = Some(t + chrono::Duration::hours(span));
        }
    }
    let mut out = Vec::new();
    if let Some((temp, wind, symbol)) = current {
        let mut line = String::from("Now: ");
        if let Some(c) = temp {
            line.push_str(&temperature(c, units));
        }
        if let Some(s) = symbol {
            line.push_str(&format!(", {}", plain(&s)));
        }
        line.push_str(&format!(", wind {}", speed(wind, units)));
        out.push(line);
    }
    let today = now.date_naive();
    for (date, d) in &days {
        let label = match (*date - today).num_days() {
            0 => "Rest of today".to_owned(),
            1 => "Tomorrow".to_owned(),
            _ => date.format("%A").to_string(),
        };
        let mut line = format!("{label}: ");
        if let (Some(h), Some(l)) = (d.high, d.low) {
            line.push_str(&format!(
                "high {}, low {}",
                temperature(h, units),
                temperature(l, units)
            ));
        }
        if let Some(s) = most_common(&d.symbols) {
            line.push_str(&format!(", mostly {s}"));
        }
        line.push_str(&format!(
            ", {}, wind up to {}",
            rain(d.rain_mm, units),
            speed(d.wind_max, units)
        ));
        out.push(line);
    }
    if out.is_empty() {
        "No forecast data came back.".into()
    } else {
        out.join("\n")
    }
}

fn temperature(c: f64, units: Units) -> String {
    match units {
        Units::Metric => format!("{:.0}°C", c),
        Units::Imperial => format!("{:.0}°F", c * 9.0 / 5.0 + 32.0),
    }
}

fn speed(ms: f64, units: Units) -> String {
    match units {
        Units::Metric => format!("{:.0} km/h", ms * 3.6),
        Units::Imperial => format!("{:.0} mph", ms * 2.236_94),
    }
}

fn rain(mm: f64, units: Units) -> String {
    if mm < 0.1 {
        return "dry".into();
    }
    match units {
        Units::Metric => format!("{mm:.1} mm of rain"),
        Units::Imperial => format!("{:.2} in of rain", mm / 25.4),
    }
}

fn most_common(items: &[String]) -> Option<String> {
    let mut counts: Vec<(&String, usize)> = Vec::new();
    for i in items {
        match counts.iter_mut().find(|(s, _)| *s == i) {
            Some(c) => c.1 += 1,
            None => counts.push((i, 1)),
        }
    }
    counts
        .into_iter()
        .max_by_key(|(_, n)| *n)
        .map(|(s, _)| s.clone())
}

/// MET Norway's symbol codes in words: "lightrainshowers_day" → "light
/// rain showers".
fn plain(code: &str) -> String {
    const WORDS: &[&str] = &[
        "partly", "cloudy", "clear", "sky", "fair", "light", "heavy", "rain", "sleet", "snow",
        "showers", "and", "thunder", "fog",
    ];
    let code = code.split('_').next().unwrap_or(code);
    let mut rest = code;
    let mut words = Vec::new();
    'outer: while !rest.is_empty() {
        for w in WORDS {
            if let Some(r) = rest.strip_prefix(w) {
                words.push(*w);
                rest = r;
                continue 'outer;
            }
        }
        // Something new: keep it whole rather than mangle it.
        return code.to_owned();
    }
    words.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn entry(
        time: &str,
        temp: f64,
        wind: f64,
        hours: &str,
        symbol: &str,
        rain: f64,
    ) -> serde_json::Value {
        serde_json::json!({
            "time": time,
            "data": {
                "instant": {"details": {"air_temperature": temp, "wind_speed": wind}},
                hours: {"summary": {"symbol_code": symbol}, "details": {"precipitation_amount": rain}}
            }
        })
    }

    #[test]
    fn symbols_read_as_words() {
        assert_eq!(plain("lightrainshowers_day"), "light rain showers");
        assert_eq!(plain("partlycloudy_night"), "partly cloudy");
        assert_eq!(plain("heavyrainandthunder"), "heavy rain and thunder");
        assert_eq!(plain("clearsky_day"), "clear sky");
        assert_eq!(plain("somethingnew"), "somethingnew");
    }

    #[test]
    fn days_by_local_date_rain_counted_once_in_the_users_units() {
        // Times are built in the test machine's local zone, so "today" and
        // "tomorrow" mean the same wherever it runs.
        let at = |day: u32, hour: u32| {
            Local
                .with_ymd_and_hms(2026, 10, day, hour, 0, 0)
                .unwrap()
                .with_timezone(&Utc)
                .format("%Y-%m-%dT%H:%M:%SZ")
                .to_string()
        };
        let now = Local.with_ymd_and_hms(2026, 10, 5, 12, 0, 0).unwrap();
        let body = serde_json::json!({"properties": {"timeseries": [
            entry(&at(5, 9), 5.0, 1.0, "next_1_hours", "clearsky_day", 0.0),   // past: skipped
            entry(&at(5, 12), 14.0, 3.0, "next_1_hours", "partlycloudy_day", 0.0),
            entry(&at(5, 13), 17.0, 5.0, "next_1_hours", "lightrain", 1.5),
            entry(&at(5, 14), 15.0, 4.0, "next_1_hours", "lightrain", 0.5),
            // 6-hourly from here; the first overlaps nothing hourly.
            entry(&at(6, 6), 9.0, 2.0, "next_6_hours", "cloudy", 3.0),
            entry(&at(6, 12), 12.0, 6.0, "next_6_hours", "cloudy", 0.0),
        ]}});
        let metric = summarise(&body, Units::Metric, now);
        let lines: Vec<&str> = metric.lines().collect();
        assert_eq!(lines[0], "Now: 14°C, partly cloudy, wind 11 km/h");
        assert_eq!(
            lines[1],
            "Rest of today: high 17°C, low 14°C, mostly light rain, 2.0 mm of rain, wind up to 18 km/h"
        );
        assert_eq!(
            lines[2],
            "Tomorrow: high 12°C, low 9°C, mostly cloudy, 3.0 mm of rain, wind up to 22 km/h"
        );
        let imperial = summarise(&body, Units::Imperial, now);
        assert!(
            imperial.starts_with("Now: 57°F, partly cloudy, wind 7 mph"),
            "{imperial}"
        );
        assert!(imperial.contains("0.08 in of rain"), "{imperial}");
    }

    #[test]
    fn coordinates_are_truncated_to_four_decimals() {
        assert_eq!(coord(40.441_694_1), 40.4416);
        assert_eq!(coord(-79.990_086_1), -79.99);
    }
}
