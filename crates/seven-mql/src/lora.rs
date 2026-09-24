//! Phase 8 `LoRa` / `Meshtastic` framing (S06 boundary).
//!
//! Budget (measured 2026-09-24 against `Meshtastic` docs + firmware limits):
//! * `LoRa` physical payload ≤ 256 B; `Meshtastic` header (16 B) + CRC (2 B) leave
//!   **237 B** for the `Meshtastic` packet.
//! * The encrypted application payload inside that packet carries at most
//!   **~239 B**, of which the *application data* budget is **~200 B**
//!   (portnum + protobuf overhead consume the rest).
//!
//! A full `themql_core::Message` is **785 B as JSON / 429 B as postcard** for
//! representative Seven evidence (measured in-tree) — it can never ride `LoRa`
//! directly. This module therefore defines a compact binary `Evidence` frame:
//!
//! ```text
//! [version:u8=1][flags:u8][hop_count:u8][payload_len:u8]
//! [evidence_id:32][observation_id:32][source_id:32]
//! [canonical_bytes:payload_len]
//! [signature:64 iff flags&0x01]
//! ```
//!
//! * `hop_count` is a *summary* of [`seven_evidence::Provenance`]: each `LoRa`
//!   relay increments it (saturating at 255). The full hop chain (node ids +
//!   timestamps) is NOT carried — it is reconstructed on reconnect via `HelixDB`
//!   `reconstruct_provenance` (S05/S09). This is documented lossy-by-design,
//!   not silent discard: the count preserves loop-prevention + staleness
//!   signals while the durable chain stays in the graph.
//! * Typical frame: 4 + 96 + 23 (canonical) + 64 (sig) = **187 B** — inside
//!   the 200 B application budget with margin.
//! * Frames that still exceed the MTU (future larger canonical shapes) use
//!   [`fragment`] / [`reassemble`] below; each fragment stays within MTU.
//!
//! Invariant 12 holds for the covered fields: decode(encode(e)) reproduces
//! `evidence_id`, `observation_id`, `source_id`, `payload`, and `signature`;
//! provenance round-trips as a hop *count*, with full-chain recovery
//! documented above.

use crate::{MqlError, MqlResult};
use seven_evidence::{Evidence, EvidenceId, ObservationId, SourceId};

/// Frame version pinned on the wire.
pub const VERSION: u8 = 1;
/// Flag bit: signature present (64 B tail).
pub const FLAG_SIGNED: u8 = 0x01;
/// `Meshtastic` application-data budget every frame must fit (bytes).
pub const LORA_APP_BUDGET: usize = 200;
/// Absolute `Meshtastic` packet payload ceiling (bytes).
pub const LORA_PACKET_MAX: usize = 237;
/// `ed25519` signature length on the wire.
pub const SIG_LEN: usize = 64;

/// Compact `LoRa` frame (binary, not hex — hex would double every id).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoraFrame {
    pub evidence_id: EvidenceId,
    pub observation_id: ObservationId,
    pub source_id: SourceId,
    /// Hop-count summary of provenance (saturating increment per relay).
    pub hop_count: u8,
    pub canonical_payload: Vec<u8>,
    pub signature: Option<Vec<u8>>,
}

impl LoraFrame {
    /// Project `Evidence` → compact frame. Provenance summarizes to a count;
    /// see module docs for the recovery path.
    #[must_use]
    pub fn from_evidence(e: &Evidence) -> Self {
        Self {
            evidence_id: e.evidence_id,
            observation_id: e.observation_id,
            source_id: e.source_id,
            hop_count: e.provenance.hops.len().min(255) as u8,
            canonical_payload: e.payload.clone(),
            signature: e.signature.clone(),
        }
    }

    /// Encode to wire bytes.
    pub fn encode(&self) -> MqlResult<Vec<u8>> {
        if self.canonical_payload.len() > 255 {
            return Err(MqlError::Lora(format!(
                "canonical payload {} B exceeds 255 B length field",
                self.canonical_payload.len()
            )));
        }
        if let Some(sig) = &self.signature
            && sig.len() != SIG_LEN
        {
            return Err(MqlError::Lora(format!(
                "signature must be {SIG_LEN} B, got {}",
                sig.len()
            )));
        }
        let flags = if self.signature.is_some() { FLAG_SIGNED } else { 0 };
        let mut out = Vec::with_capacity(4 + 96 + self.canonical_payload.len() + SIG_LEN);
        out.push(VERSION);
        out.push(flags);
        out.push(self.hop_count);
        out.push(self.canonical_payload.len() as u8);
        out.extend_from_slice(&self.evidence_id.0);
        out.extend_from_slice(&self.observation_id.0);
        out.extend_from_slice(&self.source_id.0);
        out.extend_from_slice(&self.canonical_payload);
        if let Some(sig) = &self.signature {
            out.extend_from_slice(sig);
        }
        Ok(out)
    }

    /// Decode from wire bytes.
    pub fn decode(bytes: &[u8]) -> MqlResult<Self> {
        if bytes.len() < 100 {
            return Err(MqlError::Lora(format!(
                "frame too short: {} B (min 100)",
                bytes.len()
            )));
        }
        if bytes[0] != VERSION {
            return Err(MqlError::Lora(format!(
                "unsupported version {}, expected {VERSION}",
                bytes[0]
            )));
        }
        let flags = bytes[1];
        let hop_count = bytes[2];
        let payload_len = bytes[3] as usize;
        let arr = |range: std::ops::Range<usize>| -> MqlResult<[u8; 32]> {
            bytes
                .get(range.clone())
                .and_then(|s| <[u8; 32]>::try_from(s).ok())
                .ok_or_else(|| MqlError::Lora(format!("truncated header at {range:?}")))
        };
        let evidence_id = EvidenceId(arr(4..36)?);
        let observation_id = ObservationId(arr(36..68)?);
        let source_id = SourceId(arr(68..100)?);
        let payload_end = 100usize
            .checked_add(payload_len)
            .ok_or_else(|| MqlError::Lora("payload length overflow".to_string()))?;
        let canonical_payload = bytes
            .get(100..payload_end)
            .ok_or_else(|| MqlError::Lora("truncated canonical payload".to_string()))?
            .to_vec();
        let signature = if flags & FLAG_SIGNED != 0 {
            let sig = bytes
                .get(payload_end..payload_end + SIG_LEN)
                .ok_or_else(|| MqlError::Lora("truncated signature".to_string()))?
                .to_vec();
            if bytes.len() != payload_end + SIG_LEN {
                return Err(MqlError::Lora(format!(
                    "trailing bytes: {} vs expected {}",
                    bytes.len(),
                    payload_end + SIG_LEN
                )));
            }
            Some(sig)
        } else {
            if bytes.len() != payload_end {
                return Err(MqlError::Lora(format!(
                    "trailing bytes: {} vs expected {payload_end}",
                    bytes.len()
                )));
            }
            None
        };
        Ok(Self {
            evidence_id,
            observation_id,
            source_id,
            hop_count,
            canonical_payload,
            signature,
        })
    }
}

/// One fragment of a larger frame. Each fragment's wire form is
/// `[frame_id:4][index:u8][total:u8][chunk:..]` and must stay within `mtu`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fragment {
    pub frame_id: u32,
    pub index: u8,
    pub total: u8,
    pub chunk: Vec<u8>,
}

/// Split `frame_bytes` into MTU-bounded fragments (deterministic, in order).
pub fn fragment(frame_bytes: &[u8], frame_id: u32, mtu: usize) -> MqlResult<Vec<Fragment>> {
    const HEADER: usize = 6;
    if mtu <= HEADER {
        return Err(MqlError::Lora(format!("mtu {mtu} too small for fragment header")));
    }
    let per = mtu - HEADER;
    if frame_bytes.is_empty() {
        return Err(MqlError::Lora("cannot fragment empty frame".to_string()));
    }
    let total = frame_bytes.len().div_ceil(per);
    if total > 255 {
        return Err(MqlError::Lora(format!("frame needs {total} fragments (>255)")));
    }
    Ok(frame_bytes
        .chunks(per)
        .enumerate()
        .map(|(i, c)| Fragment {
            frame_id,
            index: i as u8,
            total: total as u8,
            chunk: c.to_vec(),
        })
        .collect())
}

/// Reassemble fragments (order-independent, duplicate-tolerant) into the
/// original frame bytes. Missing fragments are an explicit error.
pub fn reassemble(frags: &[Fragment]) -> MqlResult<Vec<u8>> {
    if frags.is_empty() {
        return Err(MqlError::Lora("no fragments".to_string()));
    }
    let (fid, total) = (frags[0].frame_id, frags[0].total);
    if total == 0 {
        return Err(MqlError::Lora("fragment total is zero".to_string()));
    }
    let mut slots: Vec<Option<&Fragment>> = vec![None; total as usize];
    for f in frags {
        if f.frame_id != fid || f.total != total {
            return Err(MqlError::Lora("mixed frame ids in fragment set".to_string()));
        }
        if (f.index as usize) >= slots.len() {
            return Err(MqlError::Lora(format!("fragment index {} out of range", f.index)));
        }
        slots[f.index as usize].get_or_insert(f);
    }
    if slots.iter().any(Option::is_none) {
        return Err(MqlError::Lora("missing fragments".to_string()));
    }
    Ok(slots.into_iter().flatten().flat_map(|f| f.chunk.clone()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use seven_core::{CanonicalState, PhysicalObservation, Quaternion, SubjectId};

    fn evidence() -> Evidence {
        let state = CanonicalState::canonicalize(&PhysicalObservation {
            position_m: [100.0, 200.0, 300.0],
            velocity_ms: [1.0, 2.0, 3.0],
            attitude: Quaternion::identity(),
            observed_at_nanos: 1000,
        })
        .unwrap();
        Evidence::originate(
            &SigningKey::from_bytes(&[1; 32]),
            SubjectId("ac1".into()),
            &state,
            1000,
            i64::MAX,
        )
        .unwrap()
    }

    #[test]
    fn compact_frame_roundtrip() {
        let e = evidence();
        let f = LoraFrame::from_evidence(&e);
        let bytes = f.encode().unwrap();
        let back = LoraFrame::decode(&bytes).unwrap();
        assert_eq!(f, back);
        assert_eq!(back.evidence_id, e.evidence_id);
        assert_eq!(back.canonical_payload, e.payload);
        assert_eq!(back.signature, e.signature);
    }

    /// Payload-budget test: representative signed evidence fits the 200 B
    /// Meshtastic application budget (and trivially the 237 B packet max).
    #[test]
    fn representative_frame_fits_app_budget() {
        let bytes = LoraFrame::from_evidence(&evidence()).encode().unwrap();
        assert!(
            bytes.len() <= LORA_APP_BUDGET,
            "LoRa frame {} B exceeds {LORA_APP_BUDGET} B app budget",
            bytes.len()
        );
        assert!(bytes.len() <= LORA_PACKET_MAX);
    }

    #[test]
    fn hop_count_summarizes_provenance() {
        let mut e = evidence();
        for i in 0..300 {
            e = e.forward(seven_core::NodeId(format!("relay-{i}")), i);
        }
        let f = LoraFrame::from_evidence(&e);
        assert_eq!(f.hop_count, 255, "hop count saturates, never wraps");
    }

    #[test]
    fn fragments_roundtrip_out_of_order_with_dups() {
        let bytes = LoraFrame::from_evidence(&evidence()).encode().unwrap();
        let mut frags = fragment(&bytes, 7, 64).unwrap();
        assert!(frags.len() > 1);
        assert!(frags.iter().all(|f| f.chunk.len() + 6 <= 64));
        frags.reverse();
        frags.push(frags[0].clone()); // duplicate tolerated
        let back = reassemble(&frags).unwrap();
        assert_eq!(back, bytes);
    }

    #[test]
    fn reassembly_rejects_missing_fragment() {
        let bytes = LoraFrame::from_evidence(&evidence()).encode().unwrap();
        let frags = fragment(&bytes, 9, 64).unwrap();
        assert!(matches!(
            reassemble(&frags[1..]),
            Err(MqlError::Lora(_))
        ));
    }

    #[test]
    fn corrupt_version_rejected() {
        let mut bytes = LoraFrame::from_evidence(&evidence()).encode().unwrap();
        bytes[0] = 99;
        assert!(matches!(LoraFrame::decode(&bytes), Err(MqlError::Lora(_))));
    }
}
