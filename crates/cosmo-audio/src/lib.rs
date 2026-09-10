//! Native PipeWire audio: continuous capture into a pre-roll ring buffer,
//! VAD-based segment cuts, and playback of cached or freshly synthesized PCM.
//!
//! ## Invariants
//!
//! - A native PipeWire client, not `pw-record`: the callback rides the RT
//!   data-loop and keeps being serviced when the machine is saturated.
//! - Capture continuously (~750ms+ pre-roll) so the first syllable survives
//!   key-press latency.
//! - Half-duplex by default: the mic is held shut while speaking plus ~350ms
//!   for the room to settle, gated at the ring buffer. Barge-in stays behind
//!   config with a `doctor` warning.
