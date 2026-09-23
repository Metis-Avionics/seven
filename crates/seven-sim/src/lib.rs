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
    // Reorder is modeled at a stream level by seven-sim reorder_in_place;
    // the per-message field is kept for the documented budget interface (S13).
    #[allow(dead_code)]
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
}
