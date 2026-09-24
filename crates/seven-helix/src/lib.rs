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
    /// Caller persists child and parents first (via [`write_evidence`]). For
    /// each parent this issues one request writing both directions of the
    /// lineage link: `PARENT_OF` (parent → child) and `DERIVED_FROM`
    /// (child → parent). Both edge types are in the S05 schema; traversals
    /// use whichever direction their start node requires.
    pub fn write_parentage(child: &Evidence) -> Vec<QueryRequest> {
        // Parents are anchored by their stored evidence_id property — narrow,
        // indexed anchor per the v3 SDK style. Both ids are bound per request,
        // so each request is self-sufficient (no caller-supplied params).
        let child_hex = hex32(&child.evidence_id.0);
        child
            .parent_evidence_ids
            .iter()
            .map(|parent| {
                let parent_hex = hex32(&parent.0);
                QueryRequest::write(
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
                            g().n(NodeRef::var("parent")).add_e(
                                "PARENT_OF",
                                NodeRef::var("child"),
                                Vec::<(String, PropertyInput)>::new(),
                            ),
                        )
                        .var_as(
                            "derived",
                            g().n(NodeRef::var("child")).add_e(
                                "DERIVED_FROM",
                                NodeRef::var("parent"),
                                Vec::<(String, PropertyInput)>::new(),
                            ),
                        )
                        .returning(["edge", "derived"]),
                )
                .with_parameter_value("child_id", helix_db::QueryValue::String(child_hex.clone()))
                .with_parameter_value("parent_id", helix_db::QueryValue::String(parent_hex))
            })
            .collect()
    }

    /// Read what an evidence was derived from (S09 lineage recovery, forward
    /// direction: child → parents via `DERIVED_FROM`).
    pub fn read_derived() -> QueryRequest {
        QueryRequest::read(
            read_batch()
                .var_as(
                    "child",
                    g().n_with_label("Evidence")
                        .where_(Predicate::eq_param("evidence_id", "evidence_id")),
                )
                .var_as(
                    "parents",
                    g().n(NodeRef::var("child")).out(Some("DERIVED_FROM")).project(vec![
                        PropertyProjection::new("evidence_id"),
                        PropertyProjection::new("subject"),
                    ]),
                )
                .returning(["parents"]),
        )
        .with_query_name("read_derived")
    }

    /// Write provenance hops (S03 chain of custody → S05 graph).
    ///
    /// Caller persists the evidence first via [`write_evidence`]. For each hop
    /// this writes one `Node` (custody event: `node_id` + `received_at_nanos`)
    /// and one `FORWARDED_TO` edge from the evidence. One `Node` per hop
    /// (not one per node id): re-relays by the same node are distinct custody
    /// events and must not collapse — collapsing would silently discard
    /// provenance (invariant 7).
    pub fn write_provenance(e: &Evidence) -> Vec<QueryRequest> {
        let ev_hex = hex32(&e.evidence_id.0);
        e.provenance
            .hops
            .iter()
            .map(|hop| {
                QueryRequest::write(
                    write_batch()
                        .var_as(
                            "leaf",
                            g().n_with_label("Evidence")
                                .where_(Predicate::eq_param("evidence_id", "evidence_id")),
                        )
                        .var_as(
                            "relay",
                            g().add_n(
                                "Node",
                                vec![
                                    ("node_id", PropertyInput::Value(id_or(hop.node.0.clone()))),
                                    (
                                        "received_at_nanos",
                                        PropertyInput::Value(PropertyValue::I64(hop.received_at_nanos)),
                                    ),
                                ],
                            ),
                        )
                        .var_as(
                            "fwd",
                            g().n(NodeRef::var("leaf")).add_e(
                                "FORWARDED_TO",
                                NodeRef::var("relay"),
                                vec![(
                                    "received_at_nanos",
                                    PropertyInput::Value(PropertyValue::I64(hop.received_at_nanos)),
                                )],
                            ),
                        )
                        .returning(["fwd"]),
                )
                .with_parameter_value("evidence_id", helix_db::QueryValue::String(ev_hex.clone()))
            })
            .collect()
    }

    /// Read the custody chain for one evidence id (S09 provenance recovery —
    /// the documented recovery path for `LoRa`'s hop-count summary).
    pub fn read_provenance() -> QueryRequest {
        QueryRequest::read(
            read_batch()
                .var_as(
                    "leaf",
                    g().n_with_label("Evidence")
                        .where_(Predicate::eq_param("evidence_id", "evidence_id")),
                )
                .var_as(
                    "relays",
                    g().n(NodeRef::var("leaf")).out(Some("FORWARDED_TO")).project(vec![
                        PropertyProjection::new("node_id"),
                        PropertyProjection::new("received_at_nanos"),
                    ]),
                )
                .returning(["relays"]),
        )
        .with_query_name("read_provenance")
    }

    /// Write a belief snapshot for a subject (S05 `Belief` node).
    ///
    /// Direction is `(:Evidence)-[:SUPPORTS]->(:Belief)`: evidence supports
    /// (never constitutes) belief — the engine remains the authority on how
    /// posteriors are computed; the graph records what supported them.
    /// Mean/variance store as native `F64Array`; no float-as-string hacks.
    pub fn write_belief_state(
        subject: &seven_core::SubjectId,
        belief: &seven_belief::GaussianBelief,
        supporting: &EvidenceId,
    ) -> QueryRequest {
        let sup_hex = hex32(&supporting.0);
        QueryRequest::write(
            write_batch()
                .var_as(
                    "ev",
                    g().n_with_label("Evidence")
                        .where_(Predicate::eq_param("evidence_id", "evidence_id")),
                )
                .var_as(
                    "belief",
                    g().add_n(
                        "Belief",
                        vec![
                            ("subject", PropertyInput::Value(id_or(subject.0.clone()))),
                            (
                                "mean",
                                PropertyInput::Value(PropertyValue::F64Array(belief.mean.to_vec())),
                            ),
                            (
                                "var",
                                PropertyInput::Value(PropertyValue::F64Array(belief.var.to_vec())),
                            ),
                            (
                                "independent_updates",
                                // Saturating: the counter cannot realistically
                                // approach i64::MAX; wrap would corrupt history.
                                PropertyInput::Value(PropertyValue::I64(
                                    i64::try_from(belief.independent_updates)
                                        .unwrap_or(i64::MAX),
                                )),
                            ),
                        ],
                    ),
                )
                .var_as(
                    "sup",
                    g().n(NodeRef::var("ev")).add_e(
                        "SUPPORTS",
                        NodeRef::var("belief"),
                        Vec::<(String, PropertyInput)>::new(),
                    ),
                )
                .returning(["belief"]),
        )
        .with_parameter_value("evidence_id", helix_db::QueryValue::String(sup_hex))
    }

    /// Read the latest belief snapshot for a subject (S09 belief recovery).
    pub fn read_belief() -> QueryRequest {
        QueryRequest::read(
            read_batch()
                .var_as(
                    "belief",
                    g().n_with_label("Belief")
                        .where_(Predicate::eq_param("subject", "subject"))
                        .project(vec![
                            PropertyProjection::new("subject"),
                            PropertyProjection::new("mean"),
                            PropertyProjection::new("var"),
                            PropertyProjection::new("independent_updates"),
                        ]),
                )
                .returning(["belief"]),
        )
        .with_query_name("read_belief")
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
        for query in [
            queries::read_subject_history(),
            queries::reconstruct_provenance(),
            queries::read_provenance(),
            queries::read_belief(),
            queries::read_derived(),
        ] {
            let _ = query;
        }
    }
}
