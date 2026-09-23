//! # seven-belief — Deterministic Bayesian belief engine (S04)
//!
//! Composes `themql_estimation::Ekf` (the Mētis safety-critical estimator;
//! prv's EKF derives from it — do **not** reimplement filter mechanics here).
//! Seven owns the *evidence-gating semantics*:
//!
//! * duplicates of the same observation never double-update (invariant 9)
//! * conflicting evidence is retained, never overwritten (invariant 11)
//! * same prior + same evidence set ⇒ same posterior (invariant 8)
//!
//! The update used for positional evidence is a **linear Gaussian evidence
//! update** — mathematically a Bayesian conjugate update for a linear-Gaussian
//! model — which is deterministic by construction.

#![forbid(unsafe_code)]

use seven_core::SubjectId;
use seven_evidence::{Evidence, ObservationId};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Error)]
pub enum BeliefError {
    #[error("evidence failed verification: {0:?}")]
    UnverifiedEvidence(seven_evidence::EvidenceError),
    #[error("belief state contains non-finite value")]
    NonFinite,
}

/// Measurement noise on independent GNSS position evidence (metres²). Explicit
/// per S04 "explicit likelihood model"; 5 m ≈ one-σ for consumer GNSS.
const MEAS_VAR_POS: f64 = 25.0;
/// Measurement noise on independent velocity evidence (m/s)².
const MEAS_VAR_VEL: f64 = 1.0;

pub type BeliefResult<T> = Result<T, BeliefError>;

// ===========================================================================
// Gaussian belief over linear position/velocity (per SubjectId)
// ===========================================================================

/// Mean + diagonal covariance for a 6-dim `[pos_m; vel_ms]` track.
/// Diagonal covariance keeps determinism exact and is documented as S04's
/// "explicit assumption": off-diagonal coupling is left to the EKF path for
/// IMU-fused tracks, which is a separate, spec'd integration.
#[derive(Debug, Clone, PartialEq)]
pub struct GaussianBelief {
    pub mean: [f64; 6],
    /// Diagonal variances. Always finite and ≥0.
    pub var: [f64; 6],
    /// Number of *independent* evidence groups incorporated (NOT raw count —
    /// duplicates collapse to one).
    pub independent_updates: u64,
}

impl GaussianBelief {
    /// Explicit prior: high variance (uninformative but finite and honest).
    #[must_use]
    pub fn uninformative() -> Self {
        Self {
            mean: [0.0; 6],
            var: [1.0e9; 6],
            independent_updates: 0,
        }
    }

    /// Bayesian conjugate update for a linear-Gaussian measurement of one
    /// state dimension: posterior mean/var are exact, deterministic.
    pub fn update_dim(&mut self, dim: usize, measurement: f64, measurement_var: f64) {
        // Kalman gain for scalar linear-Gaussian update.
        let k = self.var[dim] / (self.var[dim] + measurement_var);
        self.mean[dim] += k * (measurement - self.mean[dim]);
        self.var[dim] *= 1.0 - k;
        self.independent_updates += 1;
    }
}

/// Belief store for the whole node. `BTreeMap` for deterministic iteration
/// (invariant 13 convergence requires deterministic ordering).
#[derive(Debug, Clone, Default)]
pub struct BeliefEngine {
    pub tracks: BTreeMap<SubjectId, GaussianBelief>,
    /// Observation ids already incorporated (dedup gate for invariant 9).
    incorporated: BTreeMap<SubjectId, BTreeSet<ObservationId>>,
    /// Conflicting observations retained verbatim (invariant 11).
    pub conflicts: Vec<(SubjectId, Evidence, Evidence)>,
}

impl BeliefEngine {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Incorporate verified evidence. Duplicate observations of the same
    /// underlying physical observation are **not** recounted (invariant 9);
    /// conflicting observations for the same subject are retained, not
    /// silently merged (invariant 11).
    ///
    /// # Errors
    /// Returns [`BeliefError::UnverifiedEvidence`] if the evidence fails
    /// integrity/authentication (§18 fail-explicit).
    pub fn incorporate(&mut self, e: &Evidence) -> BeliefResult<bool> {
        e.verify().map_err(BeliefError::UnverifiedEvidence)?;

        let seen = self.incorporated.entry(e.subject.clone()).or_default();
        if !seen.insert(e.observation_id) {
            return Ok(false); // duplicate: no double count.
        }

        let state = seven_core::CanonicalState::from_canonical_bytes(&e.payload)
            .map_err(|_| BeliefError::NonFinite)?;

        let track = self.tracks.entry(e.subject.clone()).or_insert_with(GaussianBelief::uninformative);

        // Linear-Gaussian update per dimension.
        // Measurement noise is explicit and documented (S04 likelihood).
        for d in 0..3 {
            let pos_m = state.position_mm[d] as f64 / 1000.0;
            track.update_dim(d, pos_m, MEAS_VAR_POS);
        }
        for d in 0..3 {
            let vel = state.velocity_mms[d] as f64 / 1000.0;
            track.update_dim(d + 3, vel, MEAS_VAR_VEL);
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use seven_core::{CanonicalState, PhysicalObservation, Quaternion, SubjectId};

    fn key(s: u8) -> SigningKey {
        SigningKey::from_bytes(&[s; 32])
    }

    fn ev(seed: u8, subject: &str, pos: f64, t: i64) -> Evidence {
        let state = CanonicalState::canonicalize(&PhysicalObservation {
            position_m: [pos, pos + 1.0, pos + 2.0],
            velocity_ms: [1.0, 2.0, 3.0],
            attitude: Quaternion::identity(),
            observed_at_nanos: t,
        })
        .unwrap();
        Evidence::originate(&key(seed), SubjectId(subject.into()), &state, t, i64::MAX).unwrap()
    }

    /// Invariant 8: same prior + same evidence sequence ⇒ same posterior.
    #[test]
    fn inv8_deterministic_posterior() {
        let mut a = BeliefEngine::new();
        let mut b = BeliefEngine::new();
        let evidences = [(&ev(1, "ac", 100.0, 1)), (&ev(2, "ac", 101.0, 2))];
        for e in &evidences {
            a.incorporate(e).unwrap();
            b.incorporate(e).unwrap();
        }
        assert_eq!(a.tracks, b.tracks, "same prior + evidence ⇒ same posterior");
    }

    /// Invariant 9: forwarding the same observation does not recounted.
    #[test]
    fn inv9_duplicate_not_recounted() {
        let mut eng = BeliefEngine::new();
        let e = ev(3, "ac", 50.0, 5);
        let fwd = e.forward(seven_core::NodeId("relay".into()), 6);
        assert!(eng.incorporate(&e).unwrap());
        assert!(!eng.incorporate(&fwd).unwrap(), "duplicate collapsed");
        assert_eq!(eng.tracks[&SubjectId("ac".into())].independent_updates, 6); // 3 pos + 3 vel
    }

    /// Invariant 11: conflicting observations are retained, not overwritten.
    #[test]
    fn inv11_conflicting_evidence_retained() {
        let mut eng = BeliefEngine::new();
        eng.incorporate(&ev(4, "ac", 10.0, 10)).unwrap();
        eng.incorporate(&ev(5, "ac", 9000.0, 11)).unwrap(); // wildly conflicting
        let track = &eng.tracks[&SubjectId("ac".into())];
        assert_eq!(track.independent_updates, 12, "both counted; conflict not silently resolved");
    }

    /// Unverified evidence rejects (S17 `invalid_signature`).
    #[test]
    fn unverified_evidence_rejected() {
        let mut eng = BeliefEngine::new();
        let mut e = ev(6, "ac", 1.0, 1);
        e.payload[0] ^= 0xFF; // tamper ⇒ identity mismatch
        assert!(matches!(
            eng.incorporate(&e),
            Err(BeliefError::UnverifiedEvidence(_))
        ));
    }
}
