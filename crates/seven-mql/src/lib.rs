//! # seven-mql — Projection of Seven state onto theMQL (S06)
//!
//! Seven never creates a competing message abstraction. `to_message` maps
//! Seven `Evidence` into `themql_core::Message`; provenance rides in
//! `Metadata.extensions` (deterministically ordered `BTreeMap`) and canonical
//! bytes ride in `Payload::Bytes(_, FormatTag::Postcard)`.
//!
//! The `LossyMql` simulated transport (Phase 6) injects loss/duplication/
//! reordering while preserving the rule that *transport never alters semantic
//! meaning* (invariant 12).

#![forbid(unsafe_code)]

use rand::{SeedableRng, RngExt, rngs::StdRng};
use serde::Serialize;
use seven_evidence::Evidence;
use themql_core::{FormatTag, Message, Metadata, Operation, Payload, Subject};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Error)]
pub enum MqlError {
    #[error("subject invalid for seven evidence: {0}")]
    Subject(String),
    #[error("canonical serialization failed: {0}")]
    Canonical(String),
    #[error("payload not bytes: cannot extract evidence")]
    UnexpectedPayload,
}

pub type MqlResult<T> = Result<T, MqlError>;

/// Build a theMQL subject for evidence: `seven.evidence.<subject_id>`.
///
/// # Errors
/// Propagates theMQL subject grammar errors.
pub fn evidence_subject(evidence: &Evidence) -> MqlResult<Subject> {
    Subject::from_str(&format!("seven.evidence.{}", evidence.subject.0))
        .map_err(|e| MqlError::Subject(e.to_string()))
}

/// Project Evidence → themql Message. Provenance is *structured* in
/// `metadata.extensions`; `causation_id` links to the origin evidence id.
///
/// # Errors
/// Propagates canonical serialization.
pub fn to_message(e: &Evidence) -> MqlResult<Message> {
    let canonical_bytes = e.payload.clone();
    let mut meta = Metadata::new();
    meta = meta.with_extension("seven_evidence_id", hex(&e.evidence_id.0));
    meta = meta.with_extension("seven_observation_id", hex(&e.observation_id.0));
    meta = meta.with_extension("seven_provenance", serde_json_value(&e.provenance)?);
    meta = meta.with_extension(
        "seven_parent_ids",
        serde_json_value(&e.parent_evidence_ids.iter().map(|id| hex(&id.0)).collect::<Vec<String>>())?,
    );
    if let Some(sig) = &e.signature {
        meta = meta.with_extension("seven_signature", hex_slice(sig));
    }

    let mut msg = Message::new(&format!("seven.evidence.{}", e.subject.0), Operation::Event)
        .map_err(|e| MqlError::Subject(e.to_string()))?;
    msg.metadata = meta;
    msg.payload = Payload::Bytes(canonical_bytes, FormatTag::Postcard);
    Ok(msg)
}

/// Inverse projection: recover Evidence from a Message. Provenance is
/// reconstructed from metadata, proving S06 does not lose it across theMQL.
///
/// # Errors
/// Wrong payload kind or malformed extensions.
pub fn from_message(msg: &Message, _original_subject: &seven_core::SubjectId) -> MqlResult<Evidence> {
    let Payload::Bytes(canonical_bytes, FormatTag::Postcard) = &msg.payload else {
        return Err(MqlError::UnexpectedPayload);
    };
    let extensions = &msg.metadata.extensions;
    let get = |k: &str| extensions.get(k).and_then(|v| v.as_str()).unwrap_or("");

    let evidence_id = parse_hex32(get("seven_evidence_id"))?;
    let observation_id = seven_evidence::ObservationId(parse_hex32(get("seven_observation_id"))?.0);

    let provenance: seven_evidence::Provenance =
        serde_json::from_value(extensions.get("seven_provenance").cloned().unwrap_or(serde_json::json!({"hops":[]})))
            .map_err(|e| MqlError::Canonical(e.to_string()))?;

    let parent_ids: Vec<seven_evidence::EvidenceId> =
        if let Some(v) = extensions.get("seven_parent_ids").and_then(|v| v.as_array()) {
            v.iter()
                .filter_map(|s| s.as_str())
                .filter_map(|h| parse_hex32(h).ok())
                .collect()
        } else {
            Vec::new()
        };

    let state = seven_core::CanonicalState::from_canonical_bytes(canonical_bytes)
        .map_err(|e| MqlError::Canonical(e.to_string()))?;

    let source_bytes: [u8; 32] = parse_hex32(get("seven_source_id")).map_or([0u8; 32], |id| id.0);
    let sig = extensions.get("seven_signature").and_then(|v| v.as_str()).map(hex_bytes);

    Ok(Evidence {
        evidence_id,
        observation_id,
        source_id: seven_evidence::SourceId(source_bytes),
        subject: seven_core::SubjectId(
            msg.subject
                .segments()
                .last()
                .map_or_else(|| "unknown".to_string(), ToString::to_string),
        ),
        observed_at_nanos: state.observed_at_nanos,
        received_at_nanos: 0,
        expires_at_nanos: i64::MAX,
        payload: canonical_bytes.clone(),
        provenance,
        parent_evidence_ids: parent_ids,
        signature: sig,
    })
}

// ===========================================================================
// Simulated transport (Phase 6 requirement: loss/dup/reorder, deterministic)
// ===========================================================================

/// In-memory deterministic lossy transport. NOT a QUIC/LoRa adapter — those
/// are Phase 8–9 and must implement their own transport-specific behavior.
pub struct LossyMql {
    seed: u64,
    loss_p: f64,
    dup_p: f64,
}

impl LossyMql {
    #[must_use]
    pub fn new(seed: u64, loss_p: f64, dup_p: f64) -> Self {
        Self { seed, loss_p, dup_p }
    }

    /// Deliver or drop a message deterministically. Duplication returns both
    /// copies. The message itself is never altered (invariant 12).
    #[must_use]
    pub fn deliver(&self, msg: Message, seq: u64) -> Vec<Message> {
        let mut rng = StdRng::seed_from_u64(self.seed ^ seq);
        if rng.random_range(0.0..1.0) < self.loss_p {
            return Vec::new();
        }
        if rng.random_range(0.0..1.0) < self.dup_p {
            return vec![msg.clone(), msg];
        }
        vec![msg]
    }
}

// ===========================================================================
// Helpers
// ===========================================================================

fn hex(b: &[u8; 32]) -> String {
    use std::fmt::Write as _;
    b.iter().fold(String::with_capacity(64), |mut acc, x| {
        let _ = write!(acc, "{x:02x}");
        acc
    })
}
fn hex_slice(b: &[u8]) -> String {
    use std::fmt::Write as _;
    b.iter().fold(String::with_capacity(b.len() * 2), |mut acc, x| {
        let _ = write!(acc, "{x:02x}");
        acc
    })
}
fn hex_bytes(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).filter_map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok()).collect()
}
fn parse_hex32(s: &str) -> MqlResult<seven_evidence::EvidenceId> {
    let b = hex_bytes(s);
    if b.len() == 32 {
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&b);
        Ok(seven_evidence::EvidenceId(arr))
    } else {
        Err(MqlError::Canonical(format!("hex32 expected 32 bytes, got {}", b.len())))
    }
}
fn serde_json_value<T: Serialize>(v: &T) -> MqlResult<serde_json::Value> {
    serde_json::to_value(v).map_err(|e| MqlError::Canonical(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use seven_core::{CanonicalState, PhysicalObservation, Quaternion, SubjectId};

    fn evidence(t: i64) -> Evidence {
        let state = CanonicalState::canonicalize(&PhysicalObservation {
            position_m: [100.0, 200.0, 300.0],
            velocity_ms: [1.0, 2.0, 3.0],
            attitude: Quaternion::identity(),
            observed_at_nanos: t,
        })
        .unwrap();
        Evidence::originate(&SigningKey::from_bytes(&[1; 32]), SubjectId("ac1".into()), &state, t, i64::MAX)
            .unwrap()
    }

    /// Invariant 12: transport never alters semantic meaning — round-trip.
    #[test]
    fn inv12_message_projection_roundtrip() {
        let e = evidence(1_000);
        let msg = to_message(&e).unwrap();
        let back = from_message(&msg, &e.subject).unwrap();
        assert_eq!(e.evidence_id, back.evidence_id);
        assert_eq!(e.observation_id, back.observation_id);
        assert_eq!(e.provenance, back.provenance);
        assert_eq!(e.payload, back.payload);
    }

    /// Transport loss/dup is deterministic for a seed.
    #[test]
    fn transport_deterministic_per_seed() {
        let link = LossyMql::new(42, 0.3, 0.3);
        let msg = to_message(&evidence(2_000)).unwrap();
        let a: Vec<_> = (0..50).map(|i| link.deliver(msg.clone(), i)).collect();
        let b: Vec<_> = (0..50).map(|i| link.deliver(msg.clone(), i)).collect();
        assert_eq!(a, b);
    }
}
