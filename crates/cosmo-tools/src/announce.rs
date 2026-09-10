//! The `announce` queue: minimum 8s spacing (plan §1.3), in-flight replies
//! wait, degrades to a notification when listening is off (notification
//! surface arrives in phase 6; phase 1 traces).

use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

/// Spacing floor from the config default (plan §1.3: ≥8s).
pub fn spacing() -> Duration {
    Duration::from_secs(8)
}

#[derive(Clone)]
pub struct Announcer {
    inner: Arc<Mutex<Inner>>,
}

struct Inner {
    last: Option<Instant>,
}

impl Default for Announcer {
    fn default() -> Self {
        Self::new()
    }
}

impl Announcer {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner { last: None })),
        }
    }

    /// Deliver a message, enforcing the ≥8s spacing by sleeping out the
    /// remainder. Phase 1 delivery is a tracing line; the spoken/notification
    /// backends slot in behind this method later.
    pub async fn announce(&self, text: &str) {
        let wait = {
            let mut inner = self.inner.lock().await;
            let wait = inner
                .last
                .map(|t| spacing().saturating_sub(t.elapsed()))
                .unwrap_or(Duration::ZERO);
            inner.last = Some(Instant::now() + wait);
            wait
        };
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
        tracing::info!(text, "announce");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn enforces_minimum_spacing() {
        let a = Announcer::new();
        let start = Instant::now();
        a.announce("one").await; // first: immediate
        assert!(start.elapsed() < Duration::from_millis(100));
        let t1 = Instant::now();
        a.announce("two").await; // second: waits out the remainder
        assert!(t1.elapsed() >= spacing().saturating_sub(Duration::from_millis(50)));
    }
}
