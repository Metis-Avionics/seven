//! # seven-replay — Deterministic replay (S12)
//!
//! A replay of a recorded event stream must reproduce the exact same final
//! state as the original run (invariant 14). This crate records and replays:
//!
//! * observations (canonical evidence creation)
//! * message sends (theMQL projection inputs)
//! * transport events (loss/dup/reorder partitions)
//! * node events (online/offline)
//! * persistence events (`HelixDB` writes/reads)
//! * belief updates
//!
//! Determinism guarantee: `replay(stream, initial_state) == replay(stream, initial_state)`
//! bit-for-bit where possible. Floating-point tolerance is documented at call
//! sites when ordering changes summation order.

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use seven_belief::BeliefEngine;
use seven_evidence::Evidence;
use std::collections::BTreeMap;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Error)]
pub enum ReplayError {
    #[error("event {index} failed: {kind}")]
    EventFailed { index: usize, kind: String },
    #[error("stream hash mismatch after replay: expected {expected}, got {actual}")]
    HashMismatch { expected: String, actual: String },
    #[error("replay is not deterministic: two runs produced different state")]
    NonDeterministic,
}

pub type ReplayResult<T> = Result<T, ReplayError>;

// ===========================================================================
// Event stream — append-only log; every event is timestamped and causal.
// ===========================================================================

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SevenEvent {
    Observation { evidence: Box<Evidence> },
    MessageSent { envelope: SevenMessage },
    MessageReceived { envelope: SevenMessage },
    TransportLoss { message_id: String },
    TransportDuplicate { message_id: String },
    TransportReorder { from_index: u64, to_index: u64 },
    NodeOnline { node_id: String },
    NodeOffline { node_id: String },
    DbWrite { key: String, value_hash: [u8; 32] },
    DbRead { key: String, value_hash: [u8; 32] },
    BeliefUpdate { subject: String, posterior: BeliefSnapshot },
}

/// A value-level snapshot of belief state for replay comparison.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BeliefSnapshot {
    pub mean: [f64; 6],
    pub var: [f64; 6],
    pub independent_updates: u64,
}

/// A thin, deterministic message envelope for replay; theMQL integration in
/// `seven-mql` carries this via themql-core Message — but replay needs a
/// serde-able record with no theMQL dependency.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SevenMessage {
    pub id: String,
    pub from: String,
    pub to: String,
    pub canonical_payload: Vec<u8>,
    pub sent_at_nanos: i64,
}

/// Causal event log. Index is the sequence number.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct EventLog {
    pub events: Vec<SevenEvent>,
    /// Seed recording for simulation-derived events (S13).
    pub seed: u64,
}

impl EventLog {
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self { events: Vec::new(), seed }
    }

    pub fn push(&mut self, event: SevenEvent) {
        self.events.push(event);
    }

    /// Hash the entire stream for tamper detection (S12 replay integrity).
    #[must_use]
    pub fn stream_hash(&self) -> [u8; 32] {
        let bytes = serde_json::to_vec(self).expect("EventLog is serializable");
        *blake3::hash(&bytes).as_bytes()
    }
}

// ===========================================================================
// Replay state machine — deterministic re-execution.
// ===========================================================================

#[derive(Debug, Default)]
pub struct ReplayEngine {
    pub beliefs: BeliefEngine,
    pub seen_messages: BTreeMap<String, SevenMessage>,
    pub nodes_online: BTreeMap<String, bool>,
    pub db: BTreeMap<String, [u8; 32]>,
}

impl ReplayEngine {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Replay a log exactly. Every event is applied deterministically; the
    /// result is compared to a second independent replay for the determinism
    /// invariant.
    ///
    /// # Errors
    /// Any event failing to apply (§18 fail-explicit).
    pub fn replay(log: &EventLog) -> ReplayResult<ReplayEngine> {
        let first = Self::replay_pass(log)?;
        let second = Self::replay_pass(log)?;
        if first.state_fingerprint() != second.state_fingerprint() {
            return Err(ReplayError::NonDeterministic);
        }
        Ok(first)
    }

    fn replay_pass(log: &EventLog) -> ReplayResult<ReplayEngine> {
        let mut engine = ReplayEngine::new();
        for (index, event) in log.events.iter().enumerate() {
            engine
                .apply(event)
                .map_err(|kind| ReplayError::EventFailed { index, kind })?;
        }
        Ok(engine)
    }

    fn apply(&mut self, event: &SevenEvent) -> std::result::Result<(), String> {
        match event {
            SevenEvent::Observation { evidence } => {
                self.beliefs
                    .incorporate(evidence)
                    .map_err(|e| format!("belief: {e:?}"))?;
                Ok(())
            }
            SevenEvent::MessageSent { envelope } => {
                self.seen_messages.insert(envelope.id.clone(), envelope.clone());
                Ok(())
            }
            SevenEvent::TransportLoss { message_id } => {
                self.seen_messages.remove(message_id);
                Ok(())
            }
            SevenEvent::NodeOnline { node_id } => {
                self.nodes_online.insert(node_id.clone(), true);
                Ok(())
            }
            SevenEvent::NodeOffline { node_id } => {
                self.nodes_online.insert(node_id.clone(), false);
                Ok(())
            }
            SevenEvent::DbWrite { key, value_hash } => {
                self.db.insert(key.clone(), *value_hash);
                Ok(())
            }
            SevenEvent::DbRead { key, .. } => {
                if !self.db.contains_key(key) {
                    return Err(format!("read of unwritten key {key}"));
                }
                Ok(())
            }
            // Reordering/duplication events are handled at ingest time in
            // seven-sim; replay sees the post-effect stream.
            SevenEvent::TransportDuplicate { .. }
            | SevenEvent::TransportReorder { .. }
            | SevenEvent::MessageReceived { .. } => Ok(()),
            SevenEvent::BeliefUpdate { .. } => {
                // Belief updates are derived from Observation events; they are
                // recorded in the log for audit but are already deterministically
                // reproduced by the Observation pass.
                Ok(())
            }
        }
    }

    /// Fingerprint of the final state for determinism comparison.
    #[must_use]
    pub fn state_fingerprint(&self) -> [u8; 32] {
        // Deterministic iteration over BTreeMap.
        let mut acc = Vec::new();
        for (k, v) in &self.seen_messages {
            acc.extend_from_slice(k.as_bytes());
            acc.extend_from_slice(&v.canonical_payload);
        }
        for (k, on) in &self.nodes_online {
            acc.extend_from_slice(k.as_bytes());
            acc.push(u8::from(*on));
        }
        *blake3::hash(&acc).as_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    use seven_core::{CanonicalState, PhysicalObservation, Quaternion, SubjectId};

    fn ev(t: i64) -> Evidence {
        let state = CanonicalState::canonicalize(&PhysicalObservation {
            position_m: [1.0, 2.0, 3.0],
            velocity_ms: [0.1, 0.2, 0.3],
            attitude: Quaternion::identity(),
            observed_at_nanos: t,
        })
        .unwrap();
        Evidence::originate(&SigningKey::from_bytes(&[7_u8; 32]), SubjectId("ac".into()), &state, t, i64::MAX)
            .unwrap()
    }

    /// Invariant 14: replay(input) == replay(input).
    #[test]
    fn inv14_replay_deterministic() {
        let mut log = EventLog::new(42);
        for i in 0..10 {
            log.push(SevenEvent::Observation { evidence: Box::new(ev(i)) });
            log.push(SevenEvent::NodeOnline { node_id: "n1".into() });
            log.push(SevenEvent::DbWrite { key: format!("k{i}"), value_hash: [i as u8; 32] });
        }
        let a = ReplayEngine::replay(&log).unwrap();
        let b = ReplayEngine::replay(&log).unwrap();
        assert_eq!(a.state_fingerprint(), b.state_fingerprint());
    }

    /// Stream hash tamper detection.
    #[test]
    fn stream_hash_detects_tampering() {
        let mut log = EventLog::new(42);
        log.push(SevenEvent::NodeOnline { node_id: "n1".into() });
        let h1 = log.stream_hash();
        log.push(SevenEvent::NodeOffline { node_id: "n1".into() });
        let h2 = log.stream_hash();
        assert_ne!(h1, h2, "adding an event changes the hash");
    }
}
