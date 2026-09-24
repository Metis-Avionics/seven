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
    #[error("EKF estimator failed: {0}")]
    Estimator(String),
}

/// Measurement noise on independent GNSS position evidence (metres²). Explicit
/// per S04 "explicit likelihood model"; 5 m ≈ one-σ for consumer GNSS.
const MEAS_VAR_POS: f64 = 25.0;
/// Measurement noise on independent velocity evidence (m/s)².
const MEAS_VAR_VEL: f64 = 1.0;
/// Conflict-flagging threshold in sigmas (decision 0004, option B). An
/// incoming measurement is flagged as conflicting iff it disagrees with the
/// pre-update posterior mean by more than this many predictive standard
/// deviations in any dimension. Named, documented, tunable — not magic.
pub const CONFLICT_SIGMA: f64 = 3.0;

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
    /// Last incorporated evidence per subject — the "previous" half of a
    /// conflict pair (decision 0004). Bounded by subject cardinality, like
    /// `tracks`.
    last_evidence: BTreeMap<SubjectId, Evidence>,
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

        // Conflict flagging (decision 0004, option B): innovation-style check
        // against the PRE-update posterior. Predictive variance `var + R`
        // (not bare `var`, which collapses toward zero and would flag every
        // subsequent observation). First observation per subject never flags
        // (no prior to disagree with); duplicates return above and never flag.
        let measurements = [
            state.position_mm[0] as f64 / 1000.0,
            state.position_mm[1] as f64 / 1000.0,
            state.position_mm[2] as f64 / 1000.0,
            state.velocity_mms[0] as f64 / 1000.0,
            state.velocity_mms[1] as f64 / 1000.0,
            state.velocity_mms[2] as f64 / 1000.0,
        ];
        let noise = [
            MEAS_VAR_POS,
            MEAS_VAR_POS,
            MEAS_VAR_POS,
            MEAS_VAR_VEL,
            MEAS_VAR_VEL,
            MEAS_VAR_VEL,
        ];
        let conflict = self.tracks.get(&e.subject).is_some_and(|track| {
            (0..6).any(|d| {
                let sigma = (track.var[d] + noise[d]).sqrt();
                (measurements[d] - track.mean[d]).abs() > CONFLICT_SIGMA * sigma
            })
        });
        if conflict
            && let Some(prev) = self.last_evidence.get(&e.subject)
        {
            self.conflicts.push((e.subject.clone(), prev.clone(), e.clone()));
        }
        self.last_evidence.insert(e.subject.clone(), e.clone());

        let track = self.tracks.entry(e.subject.clone()).or_insert_with(GaussianBelief::uninformative);

        // Linear-Gaussian update per dimension.
        // Measurement noise is explicit and documented (S04 likelihood).
        for (d, (m, r)) in measurements.iter().zip(noise.iter()).enumerate() {
            track.update_dim(d, *m, *r);
        }
        Ok(true)
    }
}

// ===========================================================================
// IMU-fused track — composes themql-estimation EKF (S04 Phase 8 wiring)
// ===========================================================================

/// An IMU-fused track for one subject. Composes `themql_estimation::Ekf`
/// directly: Seven owns evidence gating (verify + dedup + conflict retention
/// policy) while the EKF owns the 21-dim strapdown filter mechanics.
///
/// Mapping is explicit and documented (S04 "explicit likelihood/assumptions"):
/// * `CanonicalState.position_mm` → `GpsReading.position` (metres)
/// * `CanonicalState.velocity_mms` → `GpsReading.velocity` (m/s)
/// * `GpsReading.clock_bias` = 0.0 (Seven carries wall-clock in
///   `observed_at_nanos`; GPS clock bias is not estimated from ADS-B/GNSS
///   position reports)
/// * IMU propagation is caller-driven via [`ImuFusedTrack::predict`] with an
///   explicit `dt` + [`themql_estimation::ImuReading`]; Seven never invents
///   IMU samples.
///
/// Determinism: `Ekf` is fixed-size nalgebra with no RNG on the predict/update
/// path (`sample()` is never called here), so the same prior + same evidence +
/// same IMU sequence ⇒ same posterior. Duplicate `observation_id`s collapse to
/// one update (invariant 9); conflicts are counted, not silently resolved
/// (invariant 11 — the caller retains both `Evidence`s, as `BeliefEngine` does).
#[derive(Debug, Clone)]
pub struct ImuFusedTrack {
    ekf: themql_estimation::Ekf,
    incorporated: BTreeSet<ObservationId>,
    /// Number of *independent* GPS updates applied (NOT raw count).
    pub independent_updates: u64,
    /// Number of IMU predict steps applied.
    pub predict_steps: u64,
    /// Flagged conflicting pairs (decision 0004, mirrored from
    /// `BeliefEngine`: same 3-sigma rule, same likelihood). The track is
    /// single-subject, so the subject is taken from the evidence.
    pub conflicts: Vec<(SubjectId, Evidence, Evidence)>,
    /// Last incorporated evidence — the "previous" half of a conflict pair.
    last_evidence: Option<Evidence>,
}

impl ImuFusedTrack {
    #[must_use]
    pub fn new() -> Self {
        Self {
            ekf: themql_estimation::Ekf::new(),
            incorporated: BTreeSet::new(),
            independent_updates: 0,
            predict_steps: 0,
            conflicts: Vec::new(),
            last_evidence: None,
        }
    }

    /// Propagate the filter forward by `dt` seconds with an IMU reading.
    ///
    /// # Errors
    /// Maps [`themql_estimation::EstimationError`] (non-positive `dt`,
    /// non-finite IMU, diverged covariance) into [`BeliefError::Estimator`].
    pub fn predict(
        &mut self,
        dt: f64,
        imu: &themql_estimation::ImuReading,
    ) -> BeliefResult<()> {
        use themql_estimation::Estimator as _;
        self.ekf
            .predict(dt, imu)
            .map_err(|e| BeliefError::Estimator(e.to_string()))?;
        self.predict_steps += 1;
        Ok(())
    }

    /// Fuse verified position/velocity evidence as a GPS update.
    /// Returns `Ok(true)` if applied, `Ok(false)` if duplicate.
    ///
    /// Conflict flagging mirrors `BeliefEngine` (decision 0004): the incoming
    /// measurement is checked against the pre-update EKF position/velocity
    /// marginals (`state().x[0..6]`, diagonal of `state().P`) with the same
    /// `CONFLICT_SIGMA` and likelihood variances. EKF filter mechanics stay
    /// `themql-estimation`'s; Seven owns only the gating policy.
    ///
    /// # Errors
    /// Returns [`BeliefError::UnverifiedEvidence`] if integrity/auth fails,
    /// [`BeliefError::NonFinite`] if the payload is not a valid canonical
    /// state, [`BeliefError::Estimator`] if the EKF rejects the reading.
    pub fn update_from_evidence(&mut self, e: &Evidence) -> BeliefResult<bool> {
        use themql_estimation::Estimator as _;
        e.verify().map_err(BeliefError::UnverifiedEvidence)?;
        if !self.incorporated.insert(e.observation_id) {
            return Ok(false);
        }
        let state = seven_core::CanonicalState::from_canonical_bytes(&e.payload)
            .map_err(|_| BeliefError::NonFinite)?;
        let measurements = [
            state.position_mm[0] as f64 / 1000.0,
            state.position_mm[1] as f64 / 1000.0,
            state.position_mm[2] as f64 / 1000.0,
            state.velocity_mms[0] as f64 / 1000.0,
            state.velocity_mms[1] as f64 / 1000.0,
            state.velocity_mms[2] as f64 / 1000.0,
        ];
        let noise = [
            MEAS_VAR_POS,
            MEAS_VAR_POS,
            MEAS_VAR_POS,
            MEAS_VAR_VEL,
            MEAS_VAR_VEL,
            MEAS_VAR_VEL,
        ];
        let prior = self.ekf.state();
        let conflict = (0..6).any(|d| {
            let sigma = (prior.P[(d, d)] + noise[d]).sqrt();
            (measurements[d] - prior.x[d]).abs() > CONFLICT_SIGMA * sigma
        });
        if conflict
            && let Some(prev) = &self.last_evidence
        {
            self.conflicts.push((e.subject.clone(), prev.clone(), e.clone()));
        }
        self.last_evidence = Some(e.clone());
        let gps = themql_estimation::GpsReading {
            position: [measurements[0], measurements[1], measurements[2]],
            velocity: [measurements[3], measurements[4], measurements[5]],
            clock_bias: 0.0,
        };
        self.ekf
            .update_gps(&gps)
            .map_err(|e| BeliefError::Estimator(e.to_string()))?;
        self.independent_updates += 1;
        Ok(true)
    }

    /// Current EKF position estimate (metres, `[x, y, z]`).
    #[must_use]
    pub fn position(&self) -> [f64; 3] {
        use themql_estimation::Estimator as _;
        let x = self.ekf.state().x;
        [x[0], x[1], x[2]]
    }

    /// Current EKF velocity estimate (m/s, `[vx, vy, vz]`).
    #[must_use]
    pub fn velocity(&self) -> [f64; 3] {
        use themql_estimation::Estimator as _;
        let x = self.ekf.state().x;
        [x[3], x[4], x[5]]
    }

    /// Borrow the underlying estimator state (for lineage/debugging).
    #[must_use]
    pub fn estimator_state(&self) -> &themql_estimation::EstimatorState {
        use themql_estimation::Estimator as _;
        self.ekf.state()
    }
}

impl Default for ImuFusedTrack {
    fn default() -> Self {
        Self::new()
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

    /// Decision 0004 (option B): wild disagreement is flagged with the pair.
    #[test]
    fn conflict_wild_disagreement_flagged_with_pair() {
        let mut eng = BeliefEngine::new();
        let first = ev(21, "ac", 10.0, 10);
        let second = ev(22, "ac", 9000.0, 11);
        eng.incorporate(&first).unwrap();
        assert!(eng.conflicts.is_empty(), "first observation never flags");
        eng.incorporate(&second).unwrap();
        assert_eq!(eng.conflicts.len(), 1);
        let (subject, prev, new) = &eng.conflicts[0];
        assert_eq!(subject, &SubjectId("ac".into()));
        assert_eq!(prev.observation_id, first.observation_id);
        assert_eq!(new.observation_id, second.observation_id);
    }

    /// Decision 0004: agreement within sigma never flags.
    #[test]
    fn conflict_agreement_not_flagged() {
        let mut eng = BeliefEngine::new();
        eng.incorporate(&ev(23, "ac", 100.0, 1)).unwrap();
        eng.incorporate(&ev(24, "ac", 101.0, 2)).unwrap();
        eng.incorporate(&ev(25, "ac", 99.5, 3)).unwrap();
        assert!(eng.conflicts.is_empty(), "small GNSS jitter is not conflict");
    }

    /// Decision 0004: duplicates never flag, and flagging is deterministic.
    #[test]
    fn conflict_deterministic_and_duplicates_ignored() {
        let run = || {
            let mut eng = BeliefEngine::new();
            let e = ev(26, "ac", 50.0, 5);
            let fwd = e.forward(seven_core::NodeId("relay".into()), 6);
            eng.incorporate(&e).unwrap();
            eng.incorporate(&fwd).unwrap(); // duplicate: no flag
            eng.incorporate(&ev(27, "ac", 8000.0, 7)).unwrap(); // conflict
            eng.conflicts.clone()
        };
        let a = run();
        let b = run();
        assert_eq!(a, b, "same evidence ⇒ same flags");
        assert_eq!(a.len(), 1);
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

    /// EKF wiring: same evidence sequence ⇒ same fused posterior.
    /// Bitwise equality is the contract (invariant 8); `float_cmp` is allowed
    /// here because any bit divergence is a real failure.
    #[allow(clippy::float_cmp)]
    #[test]
    fn imu_track_deterministic_posterior() {
        let mut a = ImuFusedTrack::new();
        let mut b = ImuFusedTrack::new();
        for e in [&ev(11, "ac", 100.0, 1), &ev(12, "ac", 101.0, 2)] {
            a.update_from_evidence(e).unwrap();
            b.update_from_evidence(e).unwrap();
        }
        assert_eq!(a.position(), b.position());
        assert_eq!(a.velocity(), b.velocity());
        assert_eq!(a.independent_updates, 2);
    }

    /// EKF wiring: duplicates collapse; predict is explicit and counted.
    #[test]
    fn imu_track_duplicate_collapsed_predict_counted() {
        let mut t = ImuFusedTrack::new();
        let e = ev(13, "ac", 50.0, 5);
        let fwd = e.forward(seven_core::NodeId("relay".into()), 6);
        assert!(t.update_from_evidence(&e).unwrap());
        assert!(!t.update_from_evidence(&fwd).unwrap());
        assert_eq!(t.independent_updates, 1);
        let imu = themql_estimation::ImuReading {
            accel: [0.0, 0.0, 9.80665],
            gyro: [0.0, 0.0, 0.0],
        };
        t.predict(0.01, &imu).unwrap();
        assert_eq!(t.predict_steps, 1);
    }

    /// EKF wiring: invalid dt surfaces as Estimator error, not panic.
    #[test]
    fn imu_track_bad_dt_rejected() {
        let mut t = ImuFusedTrack::new();
        let imu = themql_estimation::ImuReading {
            accel: [0.0, 0.0, 0.0],
            gyro: [0.0, 0.0, 0.0],
        };
        assert!(matches!(
            t.predict(0.0, &imu),
            Err(BeliefError::Estimator(_))
        ));
    }

    /// Decision 0004 (EKF path): wild disagreement flags with the pair;
    /// agreement and duplicates stay silent; flags are deterministic.
    #[test]
    fn imu_track_conflict_flagging_mirrors_engine() {
        let mut t = ImuFusedTrack::new();
        let first = ev(31, "ac", 10.0, 10);
        let second = ev(32, "ac", 9000.0, 11);
        t.update_from_evidence(&first).unwrap();
        assert!(t.conflicts.is_empty(), "first update never flags");
        t.update_from_evidence(&second).unwrap();
        assert_eq!(t.conflicts.len(), 1);
        let (subject, prev, new) = &t.conflicts[0];
        assert_eq!(subject, &SubjectId("ac".into()));
        assert_eq!(prev.observation_id, first.observation_id);
        assert_eq!(new.observation_id, second.observation_id);

        let mut calm = ImuFusedTrack::new();
        calm.update_from_evidence(&ev(33, "ac", 100.0, 1)).unwrap();
        calm.update_from_evidence(&ev(34, "ac", 101.0, 2)).unwrap();
        assert!(calm.conflicts.is_empty(), "small jitter is not conflict");

        let run = || {
            let mut track = ImuFusedTrack::new();
            let e = ev(35, "ac", 50.0, 5);
            let fwd = e.forward(seven_core::NodeId("relay".into()), 6);
            track.update_from_evidence(&e).unwrap();
            track.update_from_evidence(&fwd).unwrap();
            track.update_from_evidence(&ev(36, "ac", 8000.0, 7)).unwrap();
            track.conflicts.clone()
        };
        assert_eq!(run(), run(), "same evidence ⇒ same flags");
        assert_eq!(run().len(), 1);
    }
}
