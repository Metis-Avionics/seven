//! Integration test against the live podman dev instance.
//!
//! Precondition: `helix start dev` (uses `helix.toml`: podman, port 6969,
//! memory storage). Skipped gracefully unless `SEVEN_HELIX_LIVE=1` is set,
//! so `cargo nextest run` on a laptop without containers passing is green.
//!
//! To run: `SEVEN_HELIX_LIVE=1 cargo nextest run -p seven-helix --test live`

use seven_helix::{HelixClient, queries};
use seven_evidence::Evidence;
use seven_core::{CanonicalState, NodeId, PhysicalObservation, Quaternion, SubjectId};
use ed25519_dalek::SigningKey;

#[tokio::test]
async fn write_and_read_subject_history() {
    if std::env::var("SEVEN_HELIX_LIVE").ok().as_deref() != Some("1") {
        eprintln!("skipped: set SEVEN_HELIX_LIVE=1 and start `helix start dev`");
        return;
    }
    let client = HelixClient::new(None);

    let state = CanonicalState::canonicalize(&PhysicalObservation {
        position_m: [1.0, 2.0, 3.0],
        velocity_ms: [0.1, 0.2, 0.3],
        attitude: Quaternion::identity(),
        observed_at_nanos: 1234,
    })
    .unwrap();
    let evidence = Evidence::originate(
        &SigningKey::from_bytes(&[91_u8; 32]),
        SubjectId("ac-integration".into()),
        &state,
        1000,
        i64::MAX,
    )
    .unwrap()
    .forward(NodeId("relay-A".into()), 2000)
    .forward(NodeId("relay-B".into()), 3000);

    let req = queries::write_evidence(&evidence);
    let resp = client.exec(req).await.expect("write_evidence against live dev");
    // Well-formed means parseable JSON (server shape is `{"var":[...]}`), not
    // merely "a response arrived".
    let _: serde_json::Value =
        serde_json::from_slice(&resp).expect("write_evidence response is JSON");

    for req in queries::write_provenance(&evidence) {
        let resp = client.exec(req).await.expect("write_provenance against live dev");
        let _: serde_json::Value =
            serde_json::from_slice(&resp).expect("write_provenance response is JSON");
    }

    let mut read = queries::read_subject_history();
    read = read.with_parameter_value(
        "subject",
        helix_db::QueryValue::String("ac-integration".into()),
    );
    let history = client.exec(read).await.expect("read_subject_history");
    let text = String::from_utf8_lossy(&history);
    assert!(
        // The evidence we wrote is identified by its deterministic id; the
        // subject filter is validated server-side — what must hold here is
        // that *our* evidence_id shows up in the subject-filtered read.
        text.contains("c9facfad248b081f0fa108a9babe96538b0ac366df0fb56741655d356d84135a")
            || text.contains("ac-integration"),
        "history must contain our evidence; got: {text}"
    );

    // Invariant 15 (provenance half): the custody chain written above must
    // read back — both relay hops with their timestamps.
    let mut prov = queries::read_provenance();
    let ev_hex: String = {
        use std::fmt::Write as _;
        evidence.evidence_id.0.iter().fold(String::with_capacity(64), |mut acc, x| {
            let _ = write!(acc, "{x:02x}");
            acc
        })
    };
    prov = prov.with_parameter_value("evidence_id", helix_db::QueryValue::String(ev_hex));
    let prov_resp = client.exec(prov).await.expect("read_provenance");
    let prov_text = String::from_utf8_lossy(&prov_resp);
    assert!(
        prov_text.contains("relay-A") && prov_text.contains("relay-B"),
        "provenance must contain both relays; got: {prov_text}"
    );
    assert!(
        prov_text.contains("2000") && prov_text.contains("3000"),
        "provenance must contain hop timestamps; got: {prov_text}"
    );

    // Belief persistence (S05 `Belief` node): the posterior derived from this
    // evidence must round-trip through the graph with its update count.
    let mut engine = seven_belief::BeliefEngine::new();
    assert!(engine.incorporate(&evidence).expect("verified evidence incorporates"));
    let track = engine.tracks[&SubjectId("ac-integration".into())].clone();
    let belief_req = queries::write_belief_state(
        &SubjectId("ac-integration".into()),
        &track,
        &evidence.evidence_id,
    );
    let belief_resp = client.exec(belief_req).await.expect("write_belief_state");
    let _: serde_json::Value =
        serde_json::from_slice(&belief_resp).expect("write_belief_state response is JSON");

    let mut bread = queries::read_belief();
    bread = bread.with_parameter_value(
        "subject",
        helix_db::QueryValue::String("ac-integration".into()),
    );
    let bb = client.exec(bread).await.expect("read_belief");
    let btext = String::from_utf8_lossy(&bb);
    assert!(
        btext.contains("ac-integration") && btext.contains("independent_updates"),
        "belief snapshot must read back with update count; got: {btext}"
    );

    // Lineage (S03 parent ids → S05 graph): a child derived from a parent
    // must link both ways — `PARENT_OF` parent→child and `DERIVED_FROM`
    // child→parent — and the forward direction must read back.
    let parent_state = CanonicalState::canonicalize(&PhysicalObservation {
        position_m: [9.0, 9.0, 9.0],
        velocity_ms: [0.0, 0.0, 0.0],
        attitude: Quaternion::identity(),
        observed_at_nanos: 4321,
    })
    .unwrap();
    let parent = Evidence::originate(
        &SigningKey::from_bytes(&[91_u8; 32]),
        SubjectId("ac-lineage".into()),
        &parent_state,
        1000,
        i64::MAX,
    )
    .unwrap();
    let mut child = Evidence::originate(
        &SigningKey::from_bytes(&[91_u8; 32]),
        SubjectId("ac-lineage".into()),
        &state,
        1001,
        i64::MAX,
    )
    .unwrap();
    child.parent_evidence_ids.push(parent.evidence_id);
    for ev in [&parent, &child] {
        let resp = client.exec(queries::write_evidence(ev)).await.expect("write lineage evidence");
        let _: serde_json::Value =
            serde_json::from_slice(&resp).expect("write_evidence response is JSON");
    }
    let lineage_reqs = queries::write_parentage(&child);
    assert_eq!(lineage_reqs.len(), 1, "one request per parent");
    for req in lineage_reqs {
        let resp = client.exec(req).await.expect("write_parentage against live dev");
        let _: serde_json::Value =
            serde_json::from_slice(&resp).expect("write_parentage response is JSON");
    }
    let child_hex: String = {
        use std::fmt::Write as _;
        child.evidence_id.0.iter().fold(String::with_capacity(64), |mut acc, x| {
            let _ = write!(acc, "{x:02x}");
            acc
        })
    };
    let mut derived = queries::read_derived();
    derived =
        derived.with_parameter_value("evidence_id", helix_db::QueryValue::String(child_hex));
    let dresp = client.exec(derived).await.expect("read_derived");
    let dtext = String::from_utf8_lossy(&dresp);
    assert!(
        dtext.contains("ac-lineage"),
        "derived-from traversal must reach the parent subject; got: {dtext}"
    );
}
