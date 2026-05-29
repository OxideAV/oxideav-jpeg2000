#![no_main]

//! Panic-free fuzz target for the MQ arithmetic decoder
//! ([`oxideav_jpeg2000::mq::MqDecoder`]) of T.800 Annex C §C.3.
//!
//! Exercises the byte-consuming engine the tier-1 Annex D coding passes
//! drive:
//!
//! * §C.3.5 INITDEC priming of the code register `C` from the first
//!   compressed byte, plus the §C.3.4 BYTEIN `0xFF`-prefixed stuff-bit
//!   rule and the `> 0x8F` end-of-stream marker (after which the
//!   decoder synthesises `0xFF00`-fill and keeps producing decisions
//!   so the residual MPS run can be decoded past the signalled byte
//!   count, per §D.4.1).
//! * §C.3.2 + Figures C.15 / C.16 / C.17 DECODE with the MPS-path /
//!   LPS-path conditional MPS/LPS exchange.
//! * §C.3.3 + Figure C.18 RENORMD shifts of `A` and `C`.
//! * §C.2.5 adaptive probability update following the Table C.2 47-row
//!   `Qe` / NMPS / NLPS / SWITCH state machine.
//!
//! The MQ decoder is **infallible** by construction (it extends the bit
//! stream rather than failing on end-of-input). The harness still drives
//! a bounded number of decisions over arbitrary attacker-controlled
//! bytes to surface any bit-shift / integer-overflow / unbounded-loop
//! corner the spec's prose doesn't make obvious. Each MQ decision is at
//! most a handful of arithmetic operations, so a single fuzz iteration
//! can drive thousands of decisions without straining the runner.
//!
//! ## Coverage shape
//!
//! Drives the four canonical Table D.7 initial contexts — `default`,
//! `uniform`, `run_length`, `zero_neighbours` — round-robin through a
//! 4-element `[MqContext; 4]` array so each context's adaptive state
//! transition is exercised on every fourth decision. The number of
//! decisions to drive is taken from the first input byte (after the
//! 1-byte cap), so libFuzzer can steer mutations toward longer / shorter
//! runs.
//!
//! ## Input cap
//!
//! The MQ decoder reads at most one byte per RENORMD shift loop, so the
//! number of input bytes bounds the entropy budget directly. Cap raw
//! input at 64 KiB.

use libfuzzer_sys::fuzz_target;
use oxideav_jpeg2000::mq::{MqContext, MqDecoder};

const MAX_INPUT_BYTES: usize = 64 * 1024;

/// Number of MQ decisions to drive on each iteration. Bounded so the
/// fuzz target completes in microseconds even on a hostile input —
/// libFuzzer schedules tens of thousands of iterations per second per
/// worker, so this is the right scale for panic discovery.
const MAX_DECISIONS: usize = 4096;

fuzz_target!(|data: &[u8]| {
    if data.is_empty() || data.len() > MAX_INPUT_BYTES {
        return;
    }

    let mut decoder = MqDecoder::new(data);

    // Four Table D.7 initial states cycled through every decision.
    let mut contexts: [MqContext; 4] = [
        MqContext::default(),
        MqContext::uniform(),
        MqContext::run_length(),
        MqContext::zero_neighbours(),
    ];

    // Steer the decision count from the first byte so libFuzzer can
    // explore both short runs (early termination, residual MPS fill)
    // and long runs (per-context probability-state convergence).
    let decisions = ((data[0] as usize) << 4).min(MAX_DECISIONS);

    for i in 0..decisions {
        let slot = i & 3;
        let _: u8 = decoder.decode(&mut contexts[slot]);
    }
});
