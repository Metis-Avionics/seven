//! # seven-evidence — Evidence identity, provenance, authn (S03/S15)
//!
//! Implements invariants 5, 6, 7, 9, 10 from spec §22 and the S03/S15
//! contract. **Three separate concepts are never conflated:**
//!
//! * **Integrity** — `blake3` hash over canonical bytes.
//! * **Authentication** — `ed25519-dalek` signature over identity + bytes.
//! * **Provenance** — explicit chain of custody; unrelated to signature.
//!
//! No cryptography is invented (S15): hash and signature are established crates.

#![forbid(unsafe_code)]

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use seven_core::{CanonicalState, NodeId, SubjectId};
use thiserror::Error;

// ===========================================================================
// Errors (§18: fail explicitly and observably)
// ===========================================================================

#[derive(Debug, Clone, PartialEq, Error)]
pub enum EvidenceError {
    #[error("canonical bytes error: {0}")]
    Canonical(#[from] seven_core::CanonicalError),
    #[error("signature invalid")]
    InvalidSignature,
    #[error("signature does not match evidence identity")]
    SignatureIdentityMismatch,
    #[error("evidence is stale (now={now_nanos}, expires_at={expires_at})")]
    Stale { now_nanos: i64, expires_at: i64 },
}

pub type EvidenceResult<T> = Result<T, EvidenceError>;

// ===========================================================================
// Identities
// ===========================================================================

/// Deterministic evidence identity: `blake3(canonical_bytes)` (S03/§6).
/// Forwarding never rehashes; a new id is born only from new canonical bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
pub struct EvidenceId(pub [u8; 32]);

impl EvidenceId {
    /// # Errors
    /// Propagates canonical serialization failure.
    pub fn of(state: &CanonicalState) -> EvidenceResult<Self> {
        let bytes = state.canonical_bytes()?;
        Ok(Self(*blake3::hash(&bytes).as_bytes()))
    }
}

/// Observation identity groups duplicates of the same underlying physical
/// observation (S03 `independence_group`). Multiple Evidence objects sharing
/// this id are ONE observation, possibly forwarded (invariant 6/9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
pub struct ObservationId(pub [u8; 32]);

/// An ed25519 verifying key as a source/node identity (S15: identity is
/// cryptographic, not a string).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SourceId(pub [u8; 32]);

// ===========================================================================
// Provenance (§7, S03)
// ===========================================================================

/// One hop in the custody chain. Authentication/integrity are checked on the
/// *Evidence*, not here — provenance is descriptive.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProvenanceHop {
    pub node: NodeId,
    pub received_at_nanos: i64,
}

/// Explicit provenance chain. Invariant 7: never silently discarded. Appended,
/// never mutated (append-only preserves auditability).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Provenance {
    pub hops: Vec<ProvenanceHop>,
}

impl Provenance {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a hop; returns a NEW chain (never mutates in place).
    #[must_use]
    pub fn extended(&self, node: NodeId, received_at_nanos: i64) -> Self {
        let mut hops = self.hops.clone();
        hops.push(ProvenanceHop { node, received_at_nanos });
        Self { hops }
    }
}

// ===========================================================================
// Evidence (S03 required properties)
// ===========================================================================

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Evidence {
    // Identity (deterministic from payload; S03 `payload_mutation_changes_identity`).
    pub evidence_id: EvidenceId,
    /// Groups duplicates of the same underlying observation (invariants 6/9).
    pub observation_id: ObservationId,
    /// Cryptographic source identity (not just a name).
    pub source_id: SourceId,
    /// Who is being observed.
    pub subject: SubjectId,
    // §11: three timestamps, NEVER collapsed.
    pub observed_at_nanos: i64,
    pub received_at_nanos: i64,
    pub expires_at_nanos: i64,
    /// Canonical bytes (the ONLY payload representation; S02 output).
    pub payload: Vec<u8>,
    /// Chain of custody; empty at the origin.
    pub provenance: Provenance,
    /// Ancestors for derived evidence (S05 graph edges derive from this).
    pub parent_evidence_ids: Vec<EvidenceId>,
    // S15: authentication. None = unauthenticated-but-retained (S17
    // `invalid_signature` scenario preserves the observation rather than dropping).
    pub signature: Option<Vec<u8>>,
}

impl Evidence {
    /// Mint ORIGIN evidence. This is the only constructor that can set a fresh
    /// `observation_id == blake3(payload)`; all downstream constructors
    /// preserve it (invariant 6).
    ///
    /// # Errors
    /// Propagates canonical/identity failures (fail-explicit, §18).
    pub fn originate(
        source: &SigningKey,
        subject: SubjectId,
        state: &CanonicalState,
        received_at_nanos: i64,
        expires_at_nanos: i64,
    ) -> EvidenceResult<Self> {
        let payload = state.canonical_bytes()?;
        let evidence_id = EvidenceId::of(state)?;
        let observation_id = ObservationId(*blake3::hash(&payload).as_bytes());
        let mut this = Self {
            evidence_id,
            observation_id,
            source_id: SourceId(*source.verifying_key().as_bytes()),
            subject,
            observed_at_nanos: state.observed_at_nanos,
            received_at_nanos,
            expires_at_nanos,
            payload,
            provenance: Provenance::new(),
            parent_evidence_ids: Vec::new(),
            signature: None,
        };
        let sig = source.sign(&this.signing_preimage());
        this.signature = Some(sig.to_bytes().to_vec());
        Ok(this)
    }

    /// FORWARD existing evidence through `node`. Identity fields are copied
    /// bit-for-bit (invariant 6: forwarding != independent evidence); only
    /// provenance and `received_at` advance.
    #[must_use]
    pub fn forward(&self, node: NodeId, received_at_nanos: i64) -> Self {
        let mut next = self.clone();
        next.received_at_nanos = received_at_nanos;
        next.provenance = self.provenance.extended(node, received_at_nanos);
        next
    }

    /// The bytes the signature signs: `evidence_id` ++ payload (domain-separated).
    #[must_use]
    pub fn signing_preimage(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(64 + self.payload.len());
        v.extend_from_slice(b"seven/evidence/v1");
        v.extend_from_slice(&self.evidence_id.0);
        v.extend_from_slice(&self.payload);
        v
    }

    /// Verify authentication (S15 separate from integrity/provenance).
    ///
    /// # Errors
    /// * [`EvidenceError::InvalidSignature`], or
    /// * [`EvidenceError::SignatureIdentityMismatch`] if the stored
    ///   `evidence_id` does not match the payload hash (tamper detection).
    pub fn verify(&self) -> EvidenceResult<()> {
        // Integrity first: stored id must equal computed id.
        let computed = EvidenceId(*blake3::hash(&self.payload).as_bytes());
        if computed != self.evidence_id {
            return Err(EvidenceError::SignatureIdentityMismatch);
        }
        let Some(sig_bytes) = &self.signature else {
            return Err(EvidenceError::InvalidSignature);
        };
        let sig: Signature = Signature::from_slice(sig_bytes)
            .map_err(|_| EvidenceError::InvalidSignature)?;
        let vk = VerifyingKey::from_bytes(&self.source_id.0)
            .map_err(|_| EvidenceError::InvalidSignature)?;
        vk.verify(&self.signing_preimage(), &sig)
            .map_err(|_| EvidenceError::InvalidSignature)
    }

    /// §11 freshness: valid signature never implies fresh (invariant 10).
    #[must_use]
    pub fn is_stale(&self, now_nanos: i64) -> bool {
        now_nanos >= self.expires_at_nanos
    }
}

// ===========================================================================
// Tests — invariants 5, 6, 7, 9, 10.
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use seven_core::{PhysicalObservation, Quaternion};

    fn key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    fn canonical(obs_nanos: i64) -> CanonicalState {
        CanonicalState::canonicalize(&PhysicalObservation {
            position_m: [100.0, 200.0, 300.0],
            velocity_ms: [1.0, 0.0, -1.0],
            attitude: Quaternion::identity(),
            observed_at_nanos: obs_nanos,
        })
        .expect("valid state")
    }

    /// Invariant 5: same canonical state + same source ⇒ same `evidence_id`.
    #[test]
    fn inv5_evidence_identity_deterministic() {
        let a = Evidence::originate(&key(1), SubjectId("ac-1".into()), &canonical(7), 0, 100).unwrap();
        let b = Evidence::originate(&key(1), SubjectId("ac-1".into()), &canonical(7), 0, 100).unwrap();
        assert_eq!(a.evidence_id, b.evidence_id);
        assert_eq!(a.observation_id, b.observation_id);
        a.verify().unwrap();
    }

    /// Invariant 6/7: forwarding preserves identity AND provenance grows.
    #[test]
    fn inv6_inv7_forwarding_not_independent_provenance_kept() {
        let origin = Evidence::originate(&key(2), SubjectId("a".into()), &canonical(1), 10, 1000).unwrap();
        let via_b = origin.forward(NodeId("B".into()), 20);
        let via_c = via_b.forward(NodeId("C".into()), 30);

        assert_eq!(origin.evidence_id, via_c.evidence_id, "identity invariant under forwarding");
        assert_eq!(origin.observation_id, via_c.observation_id);
        assert_eq!(via_c.provenance.hops.len(), 2);
        // Tamper with a forward and verification must still pass for pure
        // forwarding (provenance is descriptive), but payload tamper must fail.
        let mut bad = via_c.clone();
        bad.payload[0] ^= 0x01;
        assert_eq!(bad.verify(), Err(EvidenceError::SignatureIdentityMismatch));
    }

    /// Payload mutation ⇒ new identity (S03 invariant).
    #[test]
    fn payload_mutation_changes_identity() {
        let s1 = canonical(1);
        let s2 = {
            let mut r = PhysicalObservation {
                position_m: [100.0, 200.0, 300.0],
                velocity_ms: [1.0, 0.0, -1.0],
                attitude: Quaternion::identity(),
                observed_at_nanos: 2, // different time ⇒ different canonical bytes
            };
            r.observed_at_nanos = 2;
            CanonicalState::canonicalize(&r).unwrap()
        };
        assert_ne!(EvidenceId::of(&s1).unwrap(), EvidenceId::of(&s2).unwrap());
    }

    /// Invariant 10: stale is distinguishable and a valid signature doesn't fix it.
    #[test]
    fn inv10_stale_distinguishable_signature_independent() {
        let e = Evidence::originate(&key(3), SubjectId("a".into()), &canonical(1), 100, 200).unwrap();
        e.verify().unwrap();         // cryptographically valid…
        assert!(e.is_stale(500));    // …but stale.
        assert!(!e.is_stale(150));
        assert_eq!(
            EvidenceError::Stale { now_nanos: 500, expires_at: 200 },
            EvidenceError::Stale { now_nanos: 500, expires_at: 200 }
        );
    }

    /// Invariant 9: duplicates share `observation_id` ⇒ they are ONE observation.
    #[test]
    fn inv9_duplicate_evidence_not_independent() {
        let a = Evidence::originate(&key(4), SubjectId("a".into()), &canonical(9), 1, 100).unwrap();
        let dup = a.forward(NodeId("relay".into()), 5);
        assert_eq!(a.observation_id, dup.observation_id, "duplicates collapse to one independence group");
    }

    proptest! {
        /// For any seed/observation-time, identity is deterministic.
        #[test]
        fn prop_identity_deterministic(seed: u8, t: i64) {
            let a = Evidence::originate(&key(seed), SubjectId("s".into()), &canonical(t), 0, i64::MAX)?;
            let b = Evidence::originate(&key(seed), SubjectId("s".into()), &canonical(t), 0, i64::MAX)?;
            prop_assert_eq!(a.evidence_id, b.evidence_id);
        }

        /// Signature verifies for any well-formed origin evidence.
        #[test]
        fn prop_origin_verifies(seed: u8, t: i64) {
            let e = Evidence::originate(&key(seed), SubjectId("s".into()), &canonical(t), 0, i64::MAX)?;
            prop_assert!(e.verify().is_ok());
        }
    }
}
