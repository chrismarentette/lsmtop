//! Lane-focus seam mapping (maintainer 2026-11-10 experiment): the dim vertical
//! line marking WHERE the graph switched lanes must land in the exact
//! column the warp painted that sample into — under scroll AND squish.
use lsmtop::hist::{seam_column, squish_frame};

#[test]
fn seam_rides_the_right_edge_then_travels_left() {
    // history shorter than the frame: left-padded 1:1
    assert_eq!(seam_column(5, 0, 10, 2000), Some(9));
    assert_eq!(seam_column(5, 4, 10, 2000), Some(5));
    assert_eq!(seam_column(5, 5, 10, 2000), None, "off-screen seam honest-None");
    // plain scroll, history longer than frame: still 1:1 from the right
    assert_eq!(seam_column(500, 3, 60, 2000), Some(56));
}

#[test]
fn seam_lands_in_the_bin_the_warp_painted() {
    // squish, n > w: probe the frame with a MONOTONE index series; a bin's
    // max is its newest sample, so the seam column is the first column
    // whose value reaches (or passes) the seam's own sample index — and
    // the column to its LEFT must not contain the sample yet.
    let n = 400usize;
    let probe: Vec<f64> = (0..n).map(|i| i as f64).collect();
    let frame = squish_frame(&probe, 60, 2000);
    for back in [4usize, 40, 120, 250, 399] {
        let col = seam_column(n, back, 60, 2000).expect("seam on-screen");
        let sample = n - 1 - back;
        assert!(
            (frame[col] as usize) >= sample,
            "back={back}: col {col} ends at {}, below sample {sample}",
            frame[col]
        );
        if col > 0 {
            assert!(
                (frame[col - 1] as usize) < sample,
                "back={back}: sample {sample} also fits col {}",
                col - 1
            );
        }
    }
}
