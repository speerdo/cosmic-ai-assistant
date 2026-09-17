//! The default `stream` implementation (spec §2.2): each incoming text
//! chunk becomes one `Pcm`, in order, via `synthesize`.

use std::time::Duration;

use cosmo_tts::{Accent, LatencyClass, Pcm, TtsError, Voice, VoiceProvider};
use futures::future::{BoxFuture, ready};
use futures::stream::{BoxStream, StreamExt, iter};

struct ChunkCounter;

impl VoiceProvider for ChunkCounter {
    fn id(&self) -> &str {
        "counter"
    }

    fn list_voices(&self) -> Vec<Voice> {
        vec![Voice {
            id: "c1".into(),
            label: "Counter".into(),
            accent: Accent::from_code("en-US"),
            gender: None,
            sample: None,
        }]
    }

    fn synthesize(&self, text: &str, _voice: &str) -> BoxFuture<'_, Result<Pcm, TtsError>> {
        // Encode the chunk length as a sample count so order and 1:1-ness
        // are observable in the output.
        let n = text.len();
        Box::pin(ready(Ok(Pcm::new(1000, vec![n as f32; n]))))
    }

    fn is_local(&self) -> bool {
        true
    }

    fn latency_class(&self) -> LatencyClass {
        LatencyClass::Instant
    }
}

#[tokio::test]
async fn stream_emits_one_pcm_per_chunk_in_order() {
    let provider = ChunkCounter;
    let chunks: BoxStream<'static, String> = iter(vec![
        "first.".to_owned(),
        "second chunk.".to_owned(),
        "third.".to_owned(),
    ])
    .boxed();
    let out: Vec<Pcm> = provider
        .stream(chunks, "c1")
        .map(|r| r.expect("chunk synthesizes"))
        .collect::<Vec<_>>()
        .await;
    assert_eq!(out.len(), 3, "one Pcm per text chunk");
    let lens: Vec<usize> = out.iter().map(|p| p.data.len()).collect();
    assert_eq!(lens, vec![6, 13, 6], "chunks arrive in order");
    // Chunk lengths are also the sample counts, so durations follow.
    assert_eq!(out[0].duration(), Duration::from_millis(6));
}

#[tokio::test]
async fn stream_carries_synthesis_errors() {
    struct Failing;
    impl VoiceProvider for Failing {
        fn id(&self) -> &str {
            "failing"
        }
        fn list_voices(&self) -> Vec<Voice> {
            vec![]
        }
        fn synthesize(&self, _text: &str, _voice: &str) -> BoxFuture<'_, Result<Pcm, TtsError>> {
            Box::pin(ready(Err(TtsError::Synthesis("model exploded".into()))))
        }
        fn is_local(&self) -> bool {
            true
        }
        fn latency_class(&self) -> LatencyClass {
            LatencyClass::Instant
        }
    }
    let chunks: BoxStream<'static, String> = iter(vec!["hello".to_owned()]).boxed();
    let out: Vec<Result<Pcm, TtsError>> = Failing.stream(chunks, "v").collect().await;
    assert_eq!(out.len(), 1);
    assert!(matches!(out[0], Err(TtsError::Synthesis(_))));
}
