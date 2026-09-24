//! Phase 8 QUIC framing (S06 boundary).
//!
//! Seven does NOT extend theMQL's `TransportKind`. This module is a projection:
//! a `themql_core::Message` is serialized as JSON (`FormatTag::Json` is
//! first-class upstream) and framed with a 4-byte big-endian length prefix for
//! transport over a QUIC bidirectional stream.
//!
//! JSON (not postcard) is the framing codec here because `Message.metadata`
//! carries `serde_json::Value` extensions, which postcard cannot represent.
//! The canonical evidence bytes *inside* the message remain postcard (decision
//! 0001); only the envelope is JSON.
//!
//! QUIC socket wiring itself (quinn, TLS, congestion) is the next step after
//! this codec lands; this module pins the framing contract so socket work
//! cannot silently change semantics (invariant 12: decode(encode(m)) == m).

use crate::{MqlError, MqlResult};
use themql_core::Message;

/// Maximum framed `Message` size accepted on decode (8 MiB). QUIC has no `LoRa`
/// byte budget, but an explicit bound keeps a corrupt length prefix from
/// allocating unbounded memory (§18 fail-explicit).
pub const MAX_MESSAGE_BYTES: usize = 8 * 1024 * 1024;

/// QUIC stream configuration. Documents intent; socket wiring consumes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuicAdapterConfig {
    /// Maximum single `Message` size in bytes (JSON form).
    pub max_message_bytes: usize,
    /// Whether to keep the stream open for multiple framed messages.
    pub keep_stream_open: bool,
}

impl Default for QuicAdapterConfig {
    fn default() -> Self {
        Self {
            max_message_bytes: MAX_MESSAGE_BYTES,
            keep_stream_open: true,
        }
    }
}

/// Encode one `Message` as `[len_be32][json_bytes]`.
pub fn encode_frame(msg: &Message, config: &QuicAdapterConfig) -> MqlResult<Vec<u8>> {
    let body = serde_json::to_vec(msg)
        .map_err(|e| MqlError::Quic(format!("json encode failed: {e}")))?;
    if body.len() > config.max_message_bytes {
        return Err(MqlError::Quic(format!(
            "message {} B exceeds max {} B",
            body.len(),
            config.max_message_bytes
        )));
    }
    let mut out = Vec::with_capacity(4 + body.len());
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}

/// Decode one framed `Message` from the front of `buf`, returning the message
/// and the number of bytes consumed. `Ok(None)` means "need more bytes".
pub fn decode_frame(buf: &[u8], config: &QuicAdapterConfig) -> MqlResult<Option<(Message, usize)>> {
    if buf.len() < 4 {
        return Ok(None);
    }
    let len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    if len > config.max_message_bytes {
        return Err(MqlError::Quic(format!(
            "frame length {len} B exceeds max {} B",
            config.max_message_bytes
        )));
    }
    if buf.len() < 4 + len {
        return Ok(None);
    }
    let msg: Message = serde_json::from_slice(&buf[4..4 + len])
        .map_err(|e| MqlError::Quic(format!("json decode failed: {e}")))?;
    Ok(Some((msg, 4 + len)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use seven_core::{CanonicalState, PhysicalObservation, Quaternion, SubjectId};

    fn message() -> Message {
        let state = CanonicalState::canonicalize(&PhysicalObservation {
            position_m: [100.0, 200.0, 300.0],
            velocity_ms: [1.0, 2.0, 3.0],
            attitude: Quaternion::identity(),
            observed_at_nanos: 1000,
        })
        .unwrap();
        let e = seven_evidence::Evidence::originate(
            &SigningKey::from_bytes(&[1; 32]),
            SubjectId("ac1".into()),
            &state,
            1000,
            i64::MAX,
        )
        .unwrap();
        crate::to_message(&e, crate::SEVEN_DOMAIN).unwrap()
    }

    #[test]
    fn frame_roundtrip_preserves_message() {
        let cfg = QuicAdapterConfig::default();
        let m = message();
        let bytes = encode_frame(&m, &cfg).unwrap();
        let (back, consumed) = decode_frame(&bytes, &cfg).unwrap().expect("complete frame");
        assert_eq!(consumed, bytes.len());
        assert_eq!(m, back, "QUIC framing must preserve semantics (inv 12)");
    }

    #[test]
    fn partial_frame_waits_for_more_bytes() {
        let cfg = QuicAdapterConfig::default();
        let bytes = encode_frame(&message(), &cfg).unwrap();
        assert!(decode_frame(&bytes[..2], &cfg).unwrap().is_none());
        assert!(decode_frame(&bytes[..4], &cfg).unwrap().is_none());
        let mid = bytes.len() - 1;
        assert!(decode_frame(&bytes[..mid], &cfg).unwrap().is_none());
    }

    #[test]
    fn oversize_frame_rejected_explicitly() {
        let cfg = QuicAdapterConfig {
            max_message_bytes: 8,
            keep_stream_open: true,
        };
        assert!(matches!(
            encode_frame(&message(), &cfg),
            Err(MqlError::Quic(_))
        ));
        let bad = 1024u32.to_be_bytes();
        assert!(matches!(
            decode_frame(&bad, &cfg),
            Err(MqlError::Quic(_))
        ));
    }

    #[test]
    fn back_to_back_frames_decode_in_order() {
        let cfg = QuicAdapterConfig::default();
        let m = message();
        let a = encode_frame(&m, &cfg).unwrap();
        let b = encode_frame(&m, &cfg).unwrap();
        let mut stream = [a.clone(), b.clone()].concat();
        let (m1, n1) = decode_frame(&stream, &cfg).unwrap().unwrap();
        let (m2, n2) = decode_frame(&stream[n1..], &cfg).unwrap().unwrap();
        assert_eq!(n1 + n2, stream.len());
        assert_eq!(m1, m2);
        let _ = &mut stream;
    }
}
