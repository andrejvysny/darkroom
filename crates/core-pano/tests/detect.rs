//! Synthetic panorama-detection tests (Phase P2).
//!
//! Builds ground truth from the shared synthetic-scene machinery (`common`): a procedurally textured
//! base image viewed by a virtual camera. The rotating-camera trio is a true panorama; independent
//! seeds give burst twins (identical pose) and an unrelated frame that must not group. Fully
//! deterministic, no fixtures.

mod common;

use common::{burst_frames, three_frames, unrelated_frame};
use core_pano::{detect_groups, DetectOptions, DetectReport, EdgeClass};

/// The detection pipeline never cancels in these tests.
fn no_cancel() -> impl Fn() -> bool + Sync {
    || false
}

/// (a) A three-frame rotating sequence must form exactly one panorama group {0,1,2}, with confidence
/// above the Brown & Lowe threshold (`> 1.0`), and every verified edge classified `Pano`.
#[test]
fn pano_trio_forms_single_group() {
    let frames = three_frames([1.0, 1.0, 1.0]);
    let report = detect_groups(&frames, None, &DetectOptions::default(), &no_cancel());

    assert_eq!(report.groups.len(), 1, "groups: {:?}", report.groups);
    assert_eq!(report.groups[0].members, vec![0, 1, 2]);
    assert!(
        report.groups[0].confidence > 1.0,
        "confidence = {}",
        report.groups[0].confidence
    );
    assert!(!report.edges.is_empty(), "expected verified edges");
    assert!(
        report.edges.iter().all(|e| e.class == EdgeClass::Pano),
        "edges: {:?}",
        report.edges
    );
}

/// (b) The rotating trio plus appended burst twins: the burst edges are classified `Burst`, the twins
/// never group with each other (a burst-only component is not a group), and the trio remains the only
/// group.
#[test]
fn burst_twins_classified_and_excluded() {
    let mut frames = three_frames([1.0, 1.0, 1.0]);
    frames.extend(burst_frames()); // indices 3, 4, 5
    let report = detect_groups(&frames, None, &DetectOptions::default(), &no_cancel());

    let is_burst = |k: usize| (3..=5).contains(&k);

    // Every edge internal to the burst set is Burst, and there is at least one.
    let burst_edges: Vec<_> = report
        .edges
        .iter()
        .filter(|e| is_burst(e.i) && is_burst(e.j))
        .collect();
    assert!(!burst_edges.is_empty(), "expected burst edges among twins");
    assert!(
        burst_edges.iter().all(|e| e.class == EdgeClass::Burst),
        "burst edges: {:?}",
        burst_edges
    );

    // No burst frame joins any group, and no Pano edge connects two burst frames.
    for g in &report.groups {
        assert!(
            g.members.iter().all(|&m| !is_burst(m)),
            "burst frame grouped: {:?}",
            g
        );
    }
    assert!(
        !report
            .edges
            .iter()
            .any(|e| is_burst(e.i) && is_burst(e.j) && e.class == EdgeClass::Pano),
        "a burst-only Pano edge was formed"
    );

    // The panorama trio is still the sole group.
    assert_eq!(report.groups.len(), 1, "groups: {:?}", report.groups);
    assert_eq!(report.groups[0].members, vec![0, 1, 2]);
}

/// (c) Adding an unrelated frame: it lands in no group and no Pano edge touches it.
#[test]
fn unrelated_frame_never_groups() {
    let mut frames = three_frames([1.0, 1.0, 1.0]);
    frames.extend(burst_frames()); // 3, 4, 5
    frames.push(unrelated_frame()); // 6
    let report = detect_groups(&frames, None, &DetectOptions::default(), &no_cancel());

    assert_eq!(report.groups.len(), 1, "groups: {:?}", report.groups);
    assert_eq!(report.groups[0].members, vec![0, 1, 2]);
    for g in &report.groups {
        assert!(!g.members.contains(&6), "unrelated frame grouped: {:?}", g);
    }
    assert!(
        !report
            .edges
            .iter()
            .any(|e| (e.i == 6 || e.j == 6) && e.class == EdgeClass::Pano),
        "a Pano edge touched the unrelated frame"
    );
}

/// (d) Determinism: two invocations on the same input yield identical reports (RANSAC is seeded by
/// `pair_index`, rayon `collect` preserves order, and the report is sorted).
#[test]
fn detection_is_deterministic() {
    let mut frames = three_frames([1.0, 1.0, 1.0]);
    frames.extend(burst_frames());
    frames.push(unrelated_frame());
    let opts = DetectOptions::default();

    let r1 = detect_groups(&frames, None, &opts, &no_cancel());
    let r2 = detect_groups(&frames, None, &opts, &no_cancel());

    assert!(reports_equal(&r1, &r2), "reports differ:\n{r1:?}\n{r2:?}");
}

/// Field-exact report comparison. Determinism means bit-identical floats, so exact `==` is intended.
#[allow(clippy::float_cmp)]
fn reports_equal(a: &DetectReport, b: &DetectReport) -> bool {
    a.edges.len() == b.edges.len()
        && a.groups.len() == b.groups.len()
        && a.edges.iter().zip(&b.edges).all(|(x, y)| {
            x.i == y.i
                && x.j == y.j
                && x.conf == y.conf
                && x.overlap == y.overlap
                && x.shift == y.shift
                && x.class == y.class
        })
        && a.groups
            .iter()
            .zip(&b.groups)
            .all(|(x, y)| x.members == y.members && x.confidence == y.confidence)
}
