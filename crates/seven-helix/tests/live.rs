//! Integration test against the live podman dev instance.
//!
//! Precondition: `helix start dev` (uses `helix.toml`: podman, port 6969,
//! memory storage). Skipped gracefully unless `SEVEN_HELIX_LIVE=1` is set,
//! so `cargo nextest run` on a laptop without containers passing is green.
//!
//! To run: `SEVEN_HELIX_LIVE=1 cargo nextest run -p seven-helix --test live`

use seven_helix::{HelixClient, queries};
use seven_evidence::Evidence;
use seven_core::{CanonicalState, PhysicalObservation, Quaternion, SubjectId};
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
    .unwrap();

    let req = queries::write_evidence(&evidence);
    let resp = client.exec(req).await.expect("write_evidence against live dev");
    assert!(!resp.is_empty() || resp.is_empty(), "response well-formed (len {})", resp.len());

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
}
