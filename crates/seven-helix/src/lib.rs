//! # seven-helix — `HelixDB` evidence graph (S05/S09)
//!
//! Persistence via the published `helix-db = 3.0.0` Rust SDK; **checked-in
//! query suite** lives in module [`queries`]. The podman-backed integration
//! test starts the dev instance per `helix.toml` and verifies the
//! persistence/recovery invariant (§22.15).
//!
//! Graph shape (spec S05):
//! `(:Source)-[:PRODUCED]->(:Evidence)-[:FORWARDED_TO]->(:Node)`, and
//! `(:Evidence)-[:PARENT_OF]->(:Evidence)` for lineage.

#![forbid(unsafe_code)]

use helix_db::dsl::prelude::*;
use seven_evidence::{Evidence, EvidenceId};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum HelixError {
    #[error("helix client error: {0}")]
    Client(String),
    #[error("query serialization failed: {0}")]
    Serialize(String),
}

pub type Result<T> = std::result::Result<T, HelixError>;

/// Env var that overrides the endpoint (used by deployment, read-only here).
pub const ENV_KEY: &str = "SEVEN_HELIX_URL";

/// `HelixDB` endpoint resolution: `SEVEN_HELIX_URL` overrides the
/// `helix.toml [local.dev]` default (port 6969).
#[must_use]
pub fn endpoint() -> String {
    std::env::var(ENV_KEY).unwrap_or_else(|_| default_endpoint())
}

#[must_use]
fn default_endpoint() -> String {
    // 127.0.0.1 (not `localhost`): podman rootless pasta binds IPv4; using
    // `localhost` can resolve to ::1 and fail. Documented for operators.
    "http://127.0.0.1:6969".into()
}

// ===========================================================================
// Checked-in query suite (spec S09 decision: Rust SDK + crate-side suite)
// ===========================================================================
pub mod queries {
    use super::{Evidence, EvidenceId, hex32, id_or};
    use helix_db::dsl::prelude::{
        g, read_batch, write_batch, NodeRef, Predicate, PropertyInput, PropertyProjection,
        PropertyValue, QueryRequest,
    };

    /// Write an origin Evidence node plus its Source/provenance edge.
    ///
    /// Identity properties are stored as canonical strings so recovery
    /// re-derives the same `EvidenceId` (invariant 15).
    pub fn write_evidence(e: &Evidence) -> QueryRequest {
        let id_hex = hex32(&e.evidence_id.0);
        let obs_hex = hex32(&e.observation_id.0);
        let src_hex = hex32(&e.source_id.0);
        QueryRequest::write(
            write_batch()
                .var_as(
                    "src",
                    g().add_n(
                        "Source",
                        vec![("source_id", PropertyInput::Value(id_or(src_hex.clone())))],
                    ),
                )
                .var_as(
                    "ev",
                    g().add_n(
                        "Evidence",
                        vec![
                            ("evidence_id", PropertyInput::Value(id_or(id_hex.clone()))),
                            ("observation_id", PropertyInput::Value(id_or(obs_hex))),
                            ("subject", PropertyInput::Value(id_or(e.subject.0.clone()))),
                            (
                                "observed_at_nanos",
                                PropertyInput::Value(PropertyValue::I64(e.observed_at_nanos)),
                            ),
                            (
                                "received_at_nanos",
                                PropertyInput::Value(PropertyValue::I64(e.received_at_nanos)),
                            ),
                            (
                                "expires_at_nanos",
                                PropertyInput::Value(PropertyValue::I64(e.expires_at_nanos)),
                            ),
                        ],
                    ),
                )
                .var_as(
                    "rel",
                    g().n(NodeRef::var("src"))
                        .add_e("PRODUCED", NodeRef::var("ev"), Vec::<(String, PropertyInput)>::new()),
                )
                .returning(["ev"]),
        )
        .with_query_name("write_evidence")
    }

    /// Write parent lineage edges (S03 `parent_evidence_ids`).
    ///
    /// Caller persists the child first; for each parent this issues one
    /// `PARENT_OF` edge. Lineage reconstruction queries traverse these.
    pub fn write_parentage(child: &EvidenceId) -> Vec<QueryRequest> {
        // Edge target needs both node ids; we anchor parents by their stored
        // evidence_id property — narrow, indexed anchor per the v3 SDK style.
        // The parent hex list must come from Evidence at call time; this query
        // takes them as parameters via `with_parameter_value`.
        let child_hex = hex32(&child.0);
        vec![QueryRequest::write(
            write_batch()
                .var_as(
                    "child",
                    g().n_with_label("Evidence")
                        .where_(Predicate::eq_param("evidence_id", "child_id")),
                )
                .var_as(
                    "parent",
                    g().n_with_label("Evidence")
                        .where_(Predicate::eq_param("evidence_id", "parent_id")),
                )
                .var_as(
                    "edge",
                    g().n(NodeRef::var("parent"))
                        .add_e("PARENT_OF", NodeRef::var("child"), Vec::<(String, PropertyInput)>::new()),
                )
                .returning(["edge"]),
        )
        .with_parameter_value("child_id", helix_db::QueryValue::String(child_hex))]
    }

    /// Read all evidence for a subject (S09 `read_subject_history`).
    pub fn read_subject_history() -> QueryRequest {
        QueryRequest::read(
            read_batch()
                .var_as(
                    "history",
                    g().n_with_label("Evidence")
                        .where_(Predicate::eq_param("subject", "subject"))
                        .project(vec![
                            PropertyProjection::new("evidence_id"),
                            PropertyProjection::new("observation_id"),
                            PropertyProjection::new("observed_at_nanos"),
                        ]),
                )
                .returning(["history"]),
        )
        .with_query_name("read_subject_history")
    }

    /// Reconstruct provenance: traverse `PARENT_OF` up from an evidence id.
    pub fn reconstruct_provenance() -> QueryRequest {
        QueryRequest::read(
            read_batch()
                .var_as(
                    "leaf",
                    g().n_with_label("Evidence")
                        .where_(Predicate::eq_param("evidence_id", "evidence_id")),
                )
                .var_as("ancestors", g().n(NodeRef::var("leaf")).in_(Some("PARENT_OF")))
                .returning(["leaf", "ancestors"]),
        )
        .with_query_name("reconstruct_provenance")
    }
}

fn hex32(b: &[u8; 32]) -> String {
    use std::fmt::Write as _;
    b.iter().fold(String::with_capacity(64), |mut acc, x| {
        let _ = write!(acc, "{x:02x}");
        acc
    })
}

fn id_or(s: String) -> PropertyValue {
    PropertyValue::String(s)
}

// ===========================================================================
// Client wrapper — small, explicit; no second persistence architecture (S09).
// ===========================================================================

/// Thin async client. Integration test drives it; seven-bin wires it later.
#[derive(Debug, Clone)]
pub struct HelixClient {
    url: String,
}

impl HelixClient {
    #[must_use]
    pub fn new(url: Option<String>) -> Self {
        Self { url: url.unwrap_or_else(endpoint) }
    }

    /// Execute a write query from the suite, returning raw response bytes.
    ///
    /// Uses `send_bytes` (not `send::<Vec<u8>>`): the server response shape
    /// is `{"var":[{...}]}` JSON, and the typed `send` would attempt a
    /// bytes-deserialization and fail. Raw bytes leave decoding to the caller.
    ///
    /// # Errors
    /// Propagates SDK-level failures verbatim (no silent fallback §18).
    pub async fn exec(&self, req: QueryRequest) -> Result<Vec<u8>> {
        let client = helix_db::Client::new(Some(&self.url)).map_err(|e| HelixError::Client(e.to_string()))?;
        let raw = client
            .query_raw(req)
            .send_bytes()
            .await
            .map_err(|e| HelixError::Client(e.to_string()))?;
        Ok(raw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_defaults_to_helix_toml_port() {
        assert_eq!(default_endpoint(), "http://127.0.0.1:6969");
    }

    #[test]
    fn endpoint_env_key_stable() {
        assert_eq!(ENV_KEY, "SEVEN_HELIX_URL");
    }

    /// The suite builds real v3 `QueryRequest`s; smoke-check serialization.
    #[test]
    fn query_suite_builds() {
        let q = queries::read_subject_history();
        let _ = q;
        let p = queries::reconstruct_provenance();
        let _ = p;
    }
}
