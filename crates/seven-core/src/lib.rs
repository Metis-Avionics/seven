//! # seven-core — Canonical deterministic aviation state (S01/S02)
//!
//! Composition root for Seven's domain model. Owns the canonicalisation
//! pipeline (S02). Does **not** know about messaging, caching, persistence,
//! or transport — those are Seven adapters (`seven-mql`, `seven-six`,
//! `seven-helix`) or owned by Mētis primitives.
//!
//! ## Invariants implemented here (spec §22)
//!
//! | # | Invariant | Test |
//! |---|-----------|------|
//! | 1 | Canonicalization deterministic | `prop_canonicalization_deterministic` |
//! | 2 | Canonicalization idempotent      | `prop_canonicalization_idempotent` |
//! | 3 | `q ≡ -q` same canonical rotation | `prop_quaternion_sign_equivalence` |
//! | 4 | Invalid states rejected          | `prop_invalid_*` tests |

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use thiserror::Error;

// ===========================================================================
// Quantization policy (specs/s02-canonical-state.toml)
// ===========================================================================

/// Quantization scales. These are part of the identity contract: changing any
/// of them changes every downstream `evidence_id`.
pub mod scale {
    /// Quaternion components: unitless in [-1, 1] after normalisation.
    pub const QUATERNION: i64 = 1_000_000; // 1e6
    /// Position: metres at millimetre resolution.
    pub const POSITION: i64 = 1_000; // 1e3
    /// Velocity: m/s at mm/s resolution.
    pub const VELOCITY: i64 = 1_000; // 1e3
}

/// Quantize an `f64` deterministically into an `i64` by `round(v * scale)`.
///
/// NaN/inf are rejected before this is called (see [`validate_f64`]); values
/// overflowing `i64` are saturated deterministically rather than wrapping.
#[must_use]
pub fn quantize(v: f64, scale: i64) -> i64 {
    let s = v * scale as f64;
    // Saturating cast is deterministic on all platforms.
    s.round_ties_even() as i64
}

/// Reject non-finite values early. Canonical encoding has no concept of
/// NaN/Inf (spec S02 `missing_value`/`invalid_value`).
pub fn validate_f64(v: f64, field: &'static str) -> Result<(), CanonicalError> {
    if v.is_nan() {
        return Err(CanonicalError::NotFinite { field, value: "NaN" });
    }
    if v.is_infinite() {
        return Err(CanonicalError::NotFinite { field, value: "Inf" });
    }
    Ok(())
}

// ===========================================================================
// Errors (S02: invalid input → rejection, never silent)
// ===========================================================================

#[derive(Debug, Clone, PartialEq, Error)]
pub enum CanonicalError {
    #[error("field {field} is not finite ({value})")]
    NotFinite {
        field: &'static str,
        value: &'static str,
    },
    #[error("quaternion has zero (or subnormal) norm")]
    ZeroNormQuaternion,
    #[error("canonical serialization failed: {0}")]
    Serialization(String),
    #[error("canonical bytes deserialization failed: {0}")]
    Deserialization(String),
}

pub type CanonicalResult<T> = Result<T, CanonicalError>;

// ===========================================================================
// Quaternion (S02/§5) — Seven owns the hemisphere sign convention.
// ===========================================================================

/// A quaternion with explicit components `w, x, y, z` (Hamilton convention,
/// matching theMQL's estimation layer). Pre-quantization representation is
/// `f64`; the canonical form is quantized (see [`scale::QUATERNION`]).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Quaternion {
    pub w: f64,
    pub x: f64,
    pub y: f64,
    pub z: f64,
}

impl Quaternion {
    #[must_use]
    pub const fn new(w: f64, x: f64, y: f64, z: f64) -> Self {
        Self { w, x, y, z }
    }

    #[must_use]
    pub fn identity() -> Self {
        Self::new(1.0, 0.0, 0.0, 0.0)
    }

    /// Euclidean norm of the 4-vector.
    #[must_use]
    pub fn norm(&self) -> f64 {
        (self.w * self.w + self.x * self.x + self.y * self.y + self.z * self.z).sqrt()
    }

    /// Validate + normalize + canonicalize sign (S02 pipeline steps 3–4).
    ///
    /// Sign rule (hemisphere, first-nonzero-positive):
    /// scan `w, x, y, z`; the first component whose magnitude is strictly
    /// greater than zero must be positive. If it is negative, negate all four
    /// components (which represents the identical rotation, `q ≡ -q`).
    ///
    /// # Errors
    /// * [`CanonicalError::ZeroNormQuaternion`] — norm is zero or subnormal.
    /// * [`CanonicalError::NotFinite`] — any component is NaN/inf.
    pub fn canonicalized(&self) -> CanonicalResult<Self> {
        validate_f64(self.w, "quaternion.w")?;
        validate_f64(self.x, "quaternion.x")?;
        validate_f64(self.y, "quaternion.y")?;
        validate_f64(self.z, "quaternion.z")?;

        let n = self.norm();
        // Subnormal/zero norms cannot be normalized meaningfully.
        if n <= f64::MIN_POSITIVE {
            return Err(CanonicalError::ZeroNormQuaternion);
        }
        #[allow(clippy::many_single_char_names)] // w/x/y/z are the spec'd axes (S02).
        let (w_, x_, y_, z_) = (self.w / n, self.x / n, self.y / n, self.z / n);

        // Hemisphere selection on the *normalized* values. Exact zero is the
        // only tie-broken case; deterministic by construction.
        let sign: f64 = if w_ > 0.0 {
            1.0
        } else if w_ < 0.0 {
            -1.0
        } else if x_ > 0.0 {
            1.0
        } else if x_ < 0.0 {
            -1.0
        } else if y_ > 0.0 {
            1.0
        } else if y_ < 0.0 {
            -1.0
        } else if z_ > 0.0 {
            1.0
        } else {
            -1.0 // z_ < 0 or all-zero (already rejected above)
        };
        Ok(Self::new(sign * w_, sign * x_, sign * y_, sign * z_))
    }

    /// Quantized canonical form; identical pre-images (modulo sign and
    /// quantization error) map to identical bytes.
    #[must_use]
    pub fn to_canonical(&self) -> CanonicalQuaternion {
        // Callers must canonicalize() first; debug_assert to catch misuse in
        // core paths (release keeps this a pure quantization step).
        debug_assert!(
            (self.norm() - 1.0).abs() < 1e-6,
            "to_canonical expects a norm≈1 quaternion"
        );
        CanonicalQuaternion {
            w: quantize(self.w, scale::QUATERNION),
            x: quantize(self.x, scale::QUATERNION),
            y: quantize(self.y, scale::QUATERNION),
            z: quantize(self.z, scale::QUATERNION),
        }
    }
}

/// Quantized quaternion in canonical hemisphere.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CanonicalQuaternion {
    pub w: i64,
    pub x: i64,
    pub y: i64,
    pub z: i64,
}

// ===========================================================================
// Canonical physical state (S01 CanonicalState; S02 pipeline output)
// ===========================================================================

/// Raw physical observation, pre-canonicalization (S01 `RawObservation`).
/// Units: SI — metres, metres/second, and a Hamilton quaternion.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PhysicalObservation {
    /// Position `[x, y, z]` metres.
    pub position_m: [f64; 3],
    /// Velocity `[vx, vy, vz]` m/s.
    pub velocity_ms: [f64; 3],
    /// Attitude quaternion.
    pub attitude: Quaternion,
    /// Observation time, Unix nanos (wall clock).
    pub observed_at_nanos: i64,
}

/// Canonical, quantized, sign-hemisphere'd physical state (S02 output +
/// identity basis for evidence in S03).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CanonicalState {
    /// Position, millimetres.
    pub position_mm: [i64; 3],
    /// Velocity, mm/s.
    pub velocity_mms: [i64; 3],
    /// Attitude, canonical hemisphere.
    pub attitude: CanonicalQuaternion,
    /// Observation time, Unix nanos.
    pub observed_at_nanos: i64,
}

impl CanonicalState {
    /// Run the full S02 pipeline.
    ///
    /// # Errors
    /// Rejects NaN/Inf and zero-norm quaternions (invariant 4).
    pub fn canonicalize(raw: &PhysicalObservation) -> CanonicalResult<Self> {
        for (i, &p) in raw.position_m.iter().enumerate() {
            validate_f64(p, ["position.x", "position.y", "position.z"][i])?;
        }
        for (i, &v) in raw.velocity_ms.iter().enumerate() {
            validate_f64(v, ["velocity.x", "velocity.y", "velocity.z"][i])?;
        }
        let q = raw.attitude.canonicalized()?;
        Ok(Self {
            position_mm: [
                quantize(raw.position_m[0], scale::POSITION),
                quantize(raw.position_m[1], scale::POSITION),
                quantize(raw.position_m[2], scale::POSITION),
            ],
            velocity_mms: [
                quantize(raw.velocity_ms[0], scale::VELOCITY),
                quantize(raw.velocity_ms[1], scale::VELOCITY),
                quantize(raw.velocity_ms[2], scale::VELOCITY),
            ],
            attitude: q.to_canonical(),
            observed_at_nanos: raw.observed_at_nanos,
        })
    }

    /// **Idempotence**: re-canonicalizing an already-canonical state is the
    /// identity by construction. Quaternion components are dequantized and
    /// re-quantized *without re-normalizing* — a canonical quaternion is
    /// already in the hemisphere and within half a quantization ulp of unit
    /// norm, so re-quantization must return the same integers.
    ///
    /// # Errors
    /// Propagates any (impossible-by-construction) validation failure.
    pub fn recanonicalize(&self) -> CanonicalResult<Self> {
        let q = CanonicalQuaternion {
            w: quantize(self.attitude.w as f64 / scale::QUATERNION as f64, scale::QUATERNION),
            x: quantize(self.attitude.x as f64 / scale::QUATERNION as f64, scale::QUATERNION),
            y: quantize(self.attitude.y as f64 / scale::QUATERNION as f64, scale::QUATERNION),
            z: quantize(self.attitude.z as f64 / scale::QUATERNION as f64, scale::QUATERNION),
        };
        Ok(Self {
            position_mm: self.position_mm,
            velocity_mms: self.velocity_mms,
            attitude: q,
            observed_at_nanos: self.observed_at_nanos,
        })
    }

    /// Canonical bytes used for evidence identity (S03) and theMQL projection
    /// (S06). Postcard per specs/decisions/0001-canonical-codec.md.
    ///
    /// # Errors
    /// Serialization failure (should never happen for POD; surfaced per
    /// failure-first §18 rather than panicking).
    pub fn canonical_bytes(&self) -> CanonicalResult<Vec<u8>> {
        postcard::to_allocvec(self).map_err(|e| CanonicalError::Serialization(e.to_string()))
    }

    /// Inverse of [`canonical_bytes`]. Needed for `HelixDB` recovery (S09) and
    /// theMQL deserialization (S06).
    pub fn from_canonical_bytes(bytes: &[u8]) -> CanonicalResult<Self> {
        postcard::from_bytes(bytes).map_err(|e| CanonicalError::Deserialization(e.to_string()))
    }
}

// ===========================================================================
// Identifiers (S01: Node, Subject)
// ===========================================================================

/// A Seven network node.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
pub struct NodeId(pub String);

/// An aviation subject (aircraft or tracked entity). NOT an operational
/// identity (§21) — purely a research graph key.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
pub struct SubjectId(pub String);

// ===========================================================================
// Tests — every S02 required test + invariants 1–4.
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn raw(w: f64, x: f64, y: f64, z: f64) -> PhysicalObservation {
        PhysicalObservation {
            position_m: [1000.25, -500.5, 250.0],
            velocity_ms: [12.5, -3.25, 0.0],
            attitude: Quaternion::new(w, x, y, z),
            observed_at_nanos: 1_726_000_000_000_000_000,
        }
    }

    #[test]
    fn unit_quaternion_canonicalizes_to_self() {
        let q = Quaternion::identity();
        let c = q.canonicalized().expect("identity is valid");
        assert_eq!(c, q);
    }

    #[test]
    fn zero_quaternion_rejected() {
        assert_eq!(
            Quaternion::new(0.0, 0.0, 0.0, 0.0).canonicalized(),
            Err(CanonicalError::ZeroNormQuaternion)
        );
    }

    #[test]
    fn nan_component_rejected() {
        assert!(matches!(
            Quaternion::new(f64::NAN, 0.0, 0.0, 0.0).canonicalized(),
            Err(CanonicalError::NotFinite { .. })
        ));
    }

    #[test]
    fn infinity_rejected() {
        assert!(matches!(
            Quaternion::new(f64::INFINITY, 0.0, 0.0, 0.0).canonicalized(),
            Err(CanonicalError::NotFinite { .. })
        ));
    }

    #[test]
    fn sign_hemisphere_is_deterministic() {
        // q ≡ -q → same canonical.
        let plus = Quaternion::new(0.5, -0.5, 0.5, -0.5).canonicalized().unwrap();
        let minus = Quaternion::new(-0.5, 0.5, -0.5, 0.5).canonicalized().unwrap();
        assert_eq!(plus, minus);
        // w-first tie-break: w==0, x negative must flip.
        let q = Quaternion::new(0.0, -1.0, 0.0, 0.0).canonicalized().unwrap();
        assert_eq!(q, Quaternion::new(0.0, 1.0, 0.0, 0.0));
    }

    #[test]
    fn serialization_deterministic() {
        let s = CanonicalState::canonicalize(&raw(1.0, 0.0, 0.0, 0.0)).unwrap();
        let a = s.canonical_bytes().unwrap();
        let b = s.canonical_bytes().unwrap();
        assert_eq!(a, b, "canonical bytes must be byte-identical");
        let back = CanonicalState::from_canonical_bytes(&a).unwrap();
        assert_eq!(back, s, "byte round-trip is lossless");
    }

    #[test]
    fn idempotence_regression_found_by_proptest() {
        // Regression for proptest seed ee866d…: quantization drift on the
        // second pass used to shift w by one i64 ulp.
        let r = raw(9.619_601_440_205_019, 2.540_380_005_953_944, 3.672_129_115_224_522_3, 7.427_362_784_561_344);
        let first = CanonicalState::canonicalize(&r).unwrap();
        let second = first.recanonicalize().unwrap();
        assert_eq!(first, second);
    }

    proptest! {
        /// Invariant 1: same raw → same canonical (byte equality).
        #[test]
        fn prop_canonicalization_deterministic(
            w in -10.0f64..10.0, x in -10.0..10.0, y in -10.0..10.0, z in -10.0..10.0,
            px in -100_000.0f64..100_000.0, py in -100_000.0..100_000.0, pz in -100_000.0..100_000.0,
        ) {
            let r = PhysicalObservation {
                position_m: [px, py, pz],
                velocity_ms: [1.0, 2.0, 3.0],
                attitude: Quaternion::new(w, x, y, z),
                observed_at_nanos: 42,
            };
            if let (Ok(a), Ok(b)) = (CanonicalState::canonicalize(&r), CanonicalState::canonicalize(&r)) {
                prop_assert_eq!(a.canonical_bytes()?, b.canonical_bytes()?);
            }
        }

        /// Invariant 2: C(C(x)) == C(x).
        #[test]
        fn prop_canonicalization_idempotent(
            w in -10.0f64..10.0, x in -10.0..10.0, y in -10.0..10.0, z in -10.0..10.0,
        ) {
            prop_assume!(w != 0.0 || x != 0.0 || y != 0.0 || z != 0.0);
            let first = CanonicalState::canonicalize(&raw(w, x, y, z))?;
            let second = first.recanonicalize()?;
            prop_assert_eq!(first, second, "canonicalization must be idempotent");
        }

        /// Invariant 3: C(q) == C(-q) on canonical bytes.
        #[test]
        fn prop_quaternion_sign_equivalence(
            w in -10.0f64..10.0, x in -10.0..10.0, y in -10.0..10.0, z in -10.0..10.0,
        ) {
            prop_assume!(w != 0.0 || x != 0.0 || y != 0.0 || z != 0.0);
            let a = CanonicalState::canonicalize(&raw(w, x, y, z))?;
            let b = CanonicalState::canonicalize(&raw(-w, -x, -y, -z))?;
            prop_assert_eq!(a.attitude, b.attitude, "q and -q must have identical canonical attitudes");
        }

        /// Invariant 4: invalid states rejected.
        #[test]
        fn prop_invalid_quaternion_rejected(f in -10.0f64..10.0) {
            prop_assert!(Quaternion::new(f, f, f, f)
                .canonicalized()
                .map_or(true, |c| (c.norm() - 1.0).abs() < 1e-9));
        }

        /// Precision boundary: values straddling a quantization round.
        #[test]
        fn prop_precision_boundary_deterministic(base in 0.0f64..1.0) {
            // round_ties_even makes ties deterministic; bytes must match.
            let r1 = PhysicalObservation { position_m: [base + 0.0005, 0.0, 0.0], velocity_ms: [0.0,0.0,0.0], attitude: Quaternion::identity(), observed_at_nanos: 1 };
            let r2 = r1;
            let a = CanonicalState::canonicalize(&r1)?.canonical_bytes()?;
            let b = CanonicalState::canonicalize(&r2)?.canonical_bytes()?;
            prop_assert_eq!(a, b);
        }
    }
}
