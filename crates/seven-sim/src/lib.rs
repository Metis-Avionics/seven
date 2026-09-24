//! # seven-sim — Deterministic simulation + failure injection (S13/S17)
//!
//! Composes `prv-monte-carlo` for genuinely generic stochastic primitives
//! (seeded sampling) where its contracts apply; uses `rand::StdRng` with a
//! recorded seed for all Seven-specific failure injection so that
//! replay(seed, scenario) is exact.
//!
//! Failure events are **observable** (S17): every injected fault is logged
//! into the `EventLog` as a `SevenEvent`, never silently swallowed.

#![forbid(unsafe_code)]

use rand::{RngExt, SeedableRng, rngs::StdRng};
use seven_core::{CanonicalState, PhysicalObservation, SubjectId};
use seven_replay::{EventLog, SevenEvent, SevenMessage};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Error)]
pub enum SimError {
    #[error("invalid probability: {0} (must be in [0,1])")]
    InvalidProbability(f64),
    #[error("simulation aborted by request")]
    Aborted,
}

pub type SimResult<T> = Result<T, SimError>;

/// A deterministic lossy channel. Every injected fault is recorded in the
/// attached `EventLog` (S17 observable).
pub struct LossyChannel {
    seed: u64,
    loss_p: f64,
    dup_p: f64,
    reorder_p: f64,
}

impl LossyChannel {
    /// # Errors
    /// Rejects invalid probabilities (§18 explicit failure).
    pub fn new(seed: u64, loss_p: f64, dup_p: f64, reorder_p: f64, log: &mut EventLog) -> SimResult<Self> {
        for (name, p) in [("loss_p", loss_p), ("dup_p", dup_p), ("reorder_p", reorder_p)] {
            if !(0.0..=1.0).contains(&p) {
                return Err(SimError::InvalidProbability(p));
            }
            let _ = name;
        }
        let _ = log; // events are pushed during send()
        Ok(Self { seed, loss_p, dup_p, reorder_p })
    }

    /// Send a canonical message through the channel; returns the messages that
    /// actually arrived (post-fault). Faults are appended to `log`.
    pub fn send(
        &self,
        msg: SevenMessage,
        log: &mut EventLog,
        seq: u64,
    ) -> Vec<SevenMessage> {
        let mut rng = StdRng::seed_from_u64(self.seed ^ seq);

        // Loss.
        if rng.random_range(0.0..1.0) < self.loss_p {
            log.push(SevenEvent::TransportLoss { message_id: msg.id.clone() });
            return Vec::new();
        }

        // Duplication.
        if rng.random_range(0.0..1.0) < self.dup_p {
            log.push(SevenEvent::TransportDuplicate { message_id: msg.id.clone() });
            return vec![msg.clone(), msg];
        }

        vec![msg]
    }

    /// Send a whole stream through the channel (S13 combined fault profile).
    /// Per-message loss/dup run via [`send`] (sequence numbers `seq_base + i`),
    /// then — with probability `reorder_p`, drawn deterministically from the
    /// stream seed — arrivals are shuffled via [`reorder_in_place`], logging
    /// `TransportReorder` events. Fully deterministic per `(seed, seq_base)`,
    /// so replay stays exact. An empty stream short-circuits (no events).
    pub fn send_stream(
        &self,
        msgs: Vec<SevenMessage>,
        log: &mut EventLog,
        seq_base: u64,
    ) -> Vec<SevenMessage> {
        let mut arrivals: Vec<SevenMessage> = msgs
            .into_iter()
            .enumerate()
            .flat_map(|(i, m)| self.send(m, log, seq_base + i as u64))
            .collect();
        if arrivals.len() > 1 {
            let mut rng = StdRng::seed_from_u64(self.seed ^ seq_base ^ 0x0052_454f_5244_4552);
            if rng.random_range(0.0..1.0) < self.reorder_p {
                reorder_in_place(self.seed ^ seq_base, log, &mut arrivals);
            }
        }
        arrivals
    }
}

/// Deterministically reorder a stream of messages (seeded Fisher–Yates).
/// Every swap is logged as a `TransportReorder` event (S17 observable, never
/// swallowed). This is the stream-level reorder stage the per-message
/// [`LossyChannel`] defers to: collect arrivals, then call this before
/// delivery. Same seed ⇒ same permutation, so replay stays exact.
///
/// Indices in the logged events are stream positions at swap time.
pub fn reorder_in_place(seed: u64, log: &mut EventLog, stream: &mut [SevenMessage]) {
    let mut rng = StdRng::seed_from_u64(seed);
    for i in (1..stream.len()).rev() {
        let j = rng.random_range(0..=i);
        if i != j {
            stream.swap(i, j);
            log.push(SevenEvent::TransportReorder {
                from_index: i as u64,
                to_index: j as u64,
            });
        }
    }
}

/// Generate a stream of noisy observations (Gaussian noise, seeded).
#[must_use]
pub fn noisy_observations(
    seed: u64,
    _subject: &SubjectId,
    base: &PhysicalObservation,
    count: usize,
    noise_stddev_m: f64,
) -> Vec<CanonicalState> {
    let mut rng = StdRng::seed_from_u64(seed);
    (0..count)
        .map(|_| {
            let jitter = |sigma: f64, rng: &mut StdRng| rng.random_range(-sigma..sigma);
            let noisy = PhysicalObservation {
                position_m: [
                    base.position_m[0] + jitter(noise_stddev_m, &mut rng),
                    base.position_m[1] + jitter(noise_stddev_m, &mut rng),
                    base.position_m[2] + jitter(noise_stddev_m, &mut rng),
                ],
                velocity_ms: base.velocity_ms,
                attitude: base.attitude,
                observed_at_nanos: base.observed_at_nanos,
            };
            CanonicalState::canonicalize(&noisy).expect("noisy states must remain valid")
        })
        .collect()
}

// Compose with prv-monte-carlo where genuinely generic: `ShockSpec` applies
// to prv's economic `State` and is NOT used for aviation state (§S08
// mismatch recorded in specs/decisions/0002). Seven uses seeded rand
// directly for canonical-state perturbations.

#[cfg(test)]
mod tests {
    use super::*;
    use seven_core::NodeId;
    use seven_core::Quaternion;

    fn base() -> PhysicalObservation {
        PhysicalObservation {
            position_m: [100.0, 200.0, 300.0],
            velocity_ms: [10.0, 0.0, 0.0],
            attitude: Quaternion::identity(),
            observed_at_nanos: 1000,
        }
    }

    #[test]
    fn no_loss_no_dup_is_exact() {
        let mut log = EventLog::new(1);
        let chan = LossyChannel::new(1, 0.0, 0.0, 0.0, &mut log).unwrap();
        let msg = SevenMessage {
            id: "m1".into(),
            from: "a".into(),
            to: "b".into(),
            canonical_payload: vec![1, 2, 3],
            sent_at_nanos: 0,
        };
        let got = chan.send(msg.clone(), &mut log, 0);
        assert_eq!(got, vec![msg]);
        assert!(log.events.is_empty());
    }

    /// Deterministic: same seed + same seq ⇒ same fault stream.
    #[test]
    fn deterministic_faults() {
        let mut log = EventLog::new(9);
        let chan = LossyChannel::new(9, 0.5, 0.5, 0.5, &mut log).unwrap();
        let msg = SevenMessage {
            id: "m1".into(),
            from: "a".into(),
            to: "b".into(),
            canonical_payload: vec![],
            sent_at_nanos: 0,
        };
        let first: Vec<_> = (0..100).map(|i| chan.send(msg.clone(), &mut log, i)).collect();
        let second: Vec<_> = (0..100).map(|i| chan.send(msg.clone(), &mut log, i)).collect();
        assert_eq!(first, second);
    }

    /// Invariant 16: transport failure never corrupts canonical bytes.
    #[test]
    fn inv16_transport_failure_does_not_corrupt_canonical() {
        let state = CanonicalState::canonicalize(&base()).unwrap();
        let orig_bytes = state.canonical_bytes().unwrap();
        let mut log = EventLog::new(3);
        let chan = LossyChannel::new(3, 1.0, 1.0, 1.0, &mut log).unwrap(); // all faults
        let msg = SevenMessage {
            id: "m".into(),
            from: NodeId("a".into()).0,
            to: NodeId("b".into()).0,
            canonical_payload: orig_bytes.clone(),
            sent_at_nanos: 0,
        };
        let _ = chan.send(msg, &mut log, 0);
        // Original untouched.
        assert_eq!(orig_bytes, state.canonical_bytes().unwrap());
    }

    #[test]
    fn noisy_observations_are_deterministic() {
        let a = noisy_observations(5, &SubjectId("x".into()), &base(), 10, 1.0);
        let b = noisy_observations(5, &SubjectId("x".into()), &base(), 10, 1.0);
        assert_eq!(a, b);
    }

    #[test]
    fn invalid_probability_rejected() {
        let mut log = EventLog::new(1);
        assert!(matches!(
            LossyChannel::new(1, 1.5, 0.0, 0.0, &mut log),
            Err(SimError::InvalidProbability(_))
        ));
    }

    fn message(id: &str) -> SevenMessage {
        SevenMessage {
            id: id.into(),
            from: "a".into(),
            to: "b".into(),
            canonical_payload: vec![],
            sent_at_nanos: 0,
        }
    }

    /// Reorder is deterministic per seed and lossless (a permutation).
    #[test]
    fn reorder_deterministic_per_seed() {
        let mk = || (0..10).map(|i| message(&format!("m{i}"))).collect::<Vec<_>>();
        let mut log_a = EventLog::new(1);
        let mut first = mk();
        reorder_in_place(7, &mut log_a, &mut first);
        let mut log_b = EventLog::new(1);
        let mut second = mk();
        reorder_in_place(7, &mut log_b, &mut second);
        assert_eq!(first, second, "same seed ⇒ same permutation");
        let mut ids: Vec<_> = first.iter().map(|m| m.id.clone()).collect();
        ids.sort();
        assert_eq!(
            ids,
            (0..10).map(|i| format!("m{i}")).collect::<Vec<_>>(),
            "reorder is a permutation: nothing lost or duplicated"
        );
    }

    /// Swaps are observable: every reorder logs a `TransportReorder` event,
    /// and an empty/singleton stream logs nothing and is untouched.
    #[test]
    fn reorder_events_observable() {
        let mut log = EventLog::new(2);
        let mut empty: Vec<SevenMessage> = Vec::new();
        reorder_in_place(2, &mut log, &mut empty);
        let mut one = vec![message("solo")];
        reorder_in_place(2, &mut log, &mut one);
        assert!(log.events.is_empty());
        assert_eq!(one, vec![message("solo")]);

        let mut stream: Vec<SevenMessage> = (0..20).map(|i| message(&format!("m{i}"))).collect();
        reorder_in_place(99, &mut log, &mut stream);
        assert!(
            log.events
                .iter()
                .all(|e| matches!(e, SevenEvent::TransportReorder { .. })),
            "every logged event is a reorder"
        );
    }

    fn stream(n: usize) -> Vec<SevenMessage> {
        (0..n).map(|i| message(&format!("m{i}"))).collect()
    }

    /// `send_stream` with all faults off is the identity (and logs nothing).
    #[test]
    fn send_stream_no_faults_is_identity() {
        let mut log = EventLog::new(3);
        let chan = LossyChannel::new(3, 0.0, 0.0, 0.0, &mut log).unwrap();
        let mut log2 = EventLog::new(3);
        let got = chan.send_stream(stream(8), &mut log2, 0);
        assert_eq!(got, stream(8));
        assert!(log2.events.is_empty());
    }

    /// `send_stream` is deterministic per `(seed, seq_base)` and conserves
    /// messages modulo loss/dup: every arrival is an original (no corruption,
    /// invariant 16 at stream level).
    #[test]
    fn send_stream_deterministic_and_conserving() {
        let run = |seed: u64, base: u64| {
            let mut log = EventLog::new(seed);
            let chan = LossyChannel::new(seed, 0.4, 0.4, 1.0, &mut log).unwrap();
            (chan.send_stream(stream(12), &mut log, base), log)
        };
        let (first, log_a) = run(11, 0);
        let (second, log_b) = run(11, 0);
        assert_eq!(first, second, "same seed + base ⇒ same stream");
        assert_eq!(log_a.events, log_b.events, "same fault log");
        for m in &first {
            assert!(
                (0..12).any(|i| m.id == format!("m{i}")),
                "arrival {} is an original message",
                m.id
            );
        }
    }

    /// `reorder_p = 1` forces the shuffle stage (reorder events logged);
    /// `reorder_p = 0` never shuffles even over a long stream.
    #[test]
    fn send_stream_reorder_probability_gates_shuffle() {
        let mut log_on = EventLog::new(5);
        let chan_on = LossyChannel::new(5, 0.0, 0.0, 1.0, &mut log_on).unwrap();
        let mut log_on2 = EventLog::new(5);
        let got = chan_on.send_stream(stream(30), &mut log_on2, 0);
        assert_eq!(got.len(), 30, "no loss/dup: pure reorder stage");
        assert!(
            log_on2.events.iter().any(|e| matches!(e, SevenEvent::TransportReorder { .. })),
            "forced shuffle must log reorder events"
        );

        let mut log_off = EventLog::new(5);
        let chan_off = LossyChannel::new(5, 0.0, 0.0, 0.0, &mut log_off).unwrap();
        let mut log_off2 = EventLog::new(5);
        let same = chan_off.send_stream(stream(30), &mut log_off2, 0);
        assert_eq!(same, stream(30), "reorder_p = 0 ⇒ order preserved");
        assert!(log_off2.events.is_empty());
    }
}
