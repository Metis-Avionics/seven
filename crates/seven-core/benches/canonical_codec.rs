//! Benchmark for specs/decisions/0001-canonical-codec.md.
//!
//! This bench produces the *evidence* for the codec decision. It is kept in
//! the repo so the decision is reproducible; it is not part of CI gating.

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use seven_core::{CanonicalState, PhysicalObservation, Quaternion};

fn sample() -> PhysicalObservation {
    PhysicalObservation {
        position_m: [10_000.25, -5_000.5, 2_500.75],
        velocity_ms: [120.5, -30.25, 0.0],
        attitude: Quaternion::new(std::f64::consts::FRAC_1_SQRT_2, 0.0, std::f64::consts::FRAC_1_SQRT_2, 0.0),
        observed_at_nanos: 1_726_000_000_000_000_000,
    }
}

/// Canonical-state shape mirrored for the candidate codecs, so the benchmark
/// compares apples to apples (same fields, same quantization).
#[derive(serde::Serialize)]
#[derive(borsh::BorshSerialize)]
struct BorshCanonicalState {
    position_mm: [i64; 3],
    velocity_mms: [i64; 3],
    attitude_w: i64,
    attitude_x: i64,
    attitude_y: i64,
    attitude_z: i64,
    observed_at_nanos: i64,
}

fn to_borsh_shape(s: &CanonicalState) -> BorshCanonicalState {
    BorshCanonicalState {
        position_mm: s.position_mm,
        velocity_mms: s.velocity_mms,
        attitude_w: s.attitude.w,
        attitude_x: s.attitude.x,
        attitude_y: s.attitude.y,
        attitude_z: s.attitude.z,
        observed_at_nanos: s.observed_at_nanos,
    }
}

fn bench_codecs(c: &mut Criterion) {
    let raw = sample();
    let canonical = CanonicalState::canonicalize(&raw).expect("valid");
    let borsh_shape = to_borsh_shape(&canonical);

    let mut group = c.benchmark_group("canonical_codec");
    group.bench_function("postcard_serialize", |b| {
        b.iter(|| postcard::to_allocvec(black_box(&canonical)).unwrap());
    });
    group.bench_function("borsh_serialize", |b| {
        b.iter(|| borsh::to_vec(black_box(&borsh_shape)).unwrap());
    });

    // Size is part of the decision record (LoRa budget cares).
    let pc = postcard::to_allocvec(&canonical).unwrap();
    let bs = borsh::to_vec(&borsh_shape).unwrap();
    eprintln!("postcard bytes = {}, borsh bytes = {}", pc.len(), bs.len());
    group.finish();
}

criterion_group!(benches, bench_codecs);
criterion_main!(benches);
