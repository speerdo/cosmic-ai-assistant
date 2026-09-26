//! The `announce` queue: minimum 8s spacing (plan §1.3), in-flight replies
//! wait, degrades to a notification when speech is not available.
//!
//! *How* a message reaches the user is a [`Delivery`] the daemon supplies
//! (spec §2.7: speech when the voice layer is live, a desktop notification
//! otherwise). This module owns only the pacing, so it stays free of audio
//! and D-Bus dependencies.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

/// Where an announcement goes. Resolves when it has been delivered — for
/// speech, when the audio has finished.
pub trait Delivery: Send + Sync {
    fn deliver<'a>(&'a self, text: &'a str) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>>;
}

/// The phase-1 behavior: a tracing line and nothing else.
struct TraceOnly;

impl Delivery for TraceOnly {
    fn deliver<'a>(&'a self, text: &'a str) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
        tracing::info!(text, "announce (no delivery backend)");
        Box::pin(async {})
    }
}

/// Spacing floor from the config default (plan §1.3: ≥8s).
pub fn spacing() -> Duration {
    Duration::from_secs(8)
}

#[derive(Clone)]
pub struct Announcer {
    inner: Arc<Mutex<Inner>>,
    delivery: Arc<dyn Delivery>,
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
        Self::with_delivery(Arc::new(TraceOnly))
    }

    pub fn with_delivery(delivery: Arc<dyn Delivery>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner { last: None })),
            delivery,
        }
    }

    /// Deliver a message, enforcing the ≥8s spacing by sleeping out the
    /// remainder, then handing it to the [`Delivery`].
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
        self.delivery.deliver(text).await;
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

    #[tokio::test]
    async fn hands_each_message_to_the_delivery() {
        #[derive(Default)]
        struct Record(std::sync::Mutex<Vec<String>>);
        impl Delivery for Record {
            fn deliver<'a>(
                &'a self,
                text: &'a str,
            ) -> Pin<Box<dyn Future<Output = ()> + Send + 'a>> {
                self.0.lock().unwrap().push(text.to_owned());
                Box::pin(async {})
            }
        }
        let record = Arc::new(Record::default());
        let a = Announcer::with_delivery(record.clone());
        a.announce("build finished").await;
        assert_eq!(*record.0.lock().unwrap(), ["build finished"]);
    }
}
