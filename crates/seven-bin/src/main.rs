//! End-to-end proof: originate evidence → canonical bytes → theMQL message →
//! simulated lossy transport → belief update → deterministic state.
//!
//! Real deployments would swap in the QUIC adapter (Phase 8); the domain
//! pipeline (canonical → evidence → belief) never changes.

use seven_belief::BeliefEngine;
use seven_core::{CanonicalState, PhysicalObservation, Quaternion, SubjectId};
use seven_evidence::Evidence;
use seven_mql::{LossyMql, SEVEN_DOMAIN, from_message, to_message};
use ed25519_dalek::SigningKey;

fn main() {
    let key = SigningKey::from_bytes(&[7_u8; 32]);
    let subject = SubjectId("ac-demo".into());
    let mut engine = BeliefEngine::new();
    let link = LossyMql::new(1, 0.0, 0.0); // delivery guaranteed for demo

    for t in 0..3_i64 {
        let raw = PhysicalObservation {
            position_m: [100.0 + f64::from(t as i32), 200.0, 300.0],
            velocity_ms: [10.0, 0.0, 0.0],
            attitude: Quaternion::identity(),
            observed_at_nanos: t * 1_000,
        };
        let state = CanonicalState::canonicalize(&raw).expect("valid");
        let evidence = Evidence::originate(&key, subject.clone(), &state, t * 1_000, i64::MAX)
            .expect("evidence constructs");
        let msg = to_message(&evidence, SEVEN_DOMAIN).expect("projection");
        for arrived in link.deliver(msg, t as u64) {
            // Belief ingests what TRANSPORT delivered (S06 round-trip), not
            // the pre-transport original — loss/dup then flow through the
            // evidence dedup gate instead of being bypassed.
            let recovered = from_message(&arrived, &subject).expect("round-trip");
            let included = engine.incorporate(&recovered).expect("verified");
            println!("t={t} incorporated={included} evidence_id={:x}", evidence.evidence_id.0[0]);
        }
    }
    let track = &engine.tracks[&subject];
    println!(
        "posterior mean[0]={:.4} var[0]={:.4} independent_updates={}",
        track.mean[0], track.var[0], track.independent_updates
    );
    println!("Seven demo complete: deterministic evidence→belief pipeline green.");
}
