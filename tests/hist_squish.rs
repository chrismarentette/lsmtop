use lsmtop::hist::Ring;

// NOTE: the B6-era laws (frozen-bin quantum, stepped golden parity) were
// RETIRED 2026-11-07 by squish v3 — maintainer live: the quantum delay and the
// multi-sample-wide right edge. tests/fixtures/squish_golden.json stays on
// disk as lineage evidence only.

#[test]
fn ring_cap_90() {
    let mut ring = Ring::new(90);
    for i in 0..200 {
        ring.push(i as f64);
    }
    let vals = ring.values();
    assert_eq!(vals.len(), 90);
    assert_eq!(vals[0], 110.0);
    assert_eq!(vals[89], 199.0);
}

#[test]
fn squish_properties() {
    let mut raw = vec![0.0; 200];
    raw[40] = 100.0;
    
    
    for tick in [0u64, 500, 2000, 5000] {
        let frame = lsmtop::hist::squish_frame(&raw, 60, tick);
        assert_eq!(frame.len(), 60);
        let mut max_val = 0.0;
        let mut max_count = 0;
        for &v in &frame {
            if v > max_val {
                max_val = v;
                max_count = 1;
            } else if (v - max_val).abs() < 1e-9 {
                max_count += 1;
            }
        }
        assert!((max_val - 100.0).abs() < 1e-9, "blinked out at tick {}", tick);
        assert_eq!(max_count, 1, "appeared {} times at tick {}", max_count, tick);
    }
    
    // monotone
    let mono = (0..200).map(|x| x as f64).collect::<Vec<_>>();
    let frame = lsmtop::hist::squish_frame(&mono, 60, 2000);
    for w in frame.windows(2) {
        assert!(w[0] <= w[1] + 1e-9, "not monotone: {} > {}", w[0], w[1]);
    }
    
    // all-zero
    let zeros = vec![0.0; 200];
    let frame = lsmtop::hist::squish_frame(&zeros, 60, 2000);
    for v in frame {
        assert_eq!(v, 0.0);
    }
}

use lsmtop::engine::leaky::LeakyMax;

#[test]
fn to_columns_normalization() {
    let series = vec![10.0, 50.0, 100.0];
    
    // Fixed branch
    let (out_fixed, vmax_fixed) = lsmtop::hist::to_columns(&series, 10, 10, lsmtop::hist::Norm::Fixed(100.0));
    assert_eq!(vmax_fixed, 100.0);
    assert_eq!(out_fixed, vec![0.1, 0.5, 1.0]);
    
    // Leaky branch
    let mut leaky = LeakyMax::new(0.997);
    let (out_leaky, vmax_leaky) = lsmtop::hist::to_columns(&series, 10, 10, lsmtop::hist::Norm::Leaky(&mut leaky, 60));
    
    // t = 100 * 1.05 + 0.5 = 105.5
    // eighth = 100 / 8 = 12.5
    // log10(12.5) = 1.096 -> floor = 1.0 -> e = 10.0
    // mults * 10: 10, 12.5, 15, 20, 25, 50, 100
    // smallest >= 12.5 is 12.5 (m=1.25). So step = 12.5
    // ceil(105.5 / 12.5) = ceil(8.44) = 9
    // 9 * 12.5 = 112.5
    assert!((vmax_leaky - 112.5).abs() < 1e-9, "expected 112.5 got {}", vmax_leaky);
    assert_eq!(leaky.v, 100.0);
    
    assert!((out_leaky[2] - (100.0 / 112.5)).abs() < 1e-9);
}


#[test]
fn squish_completeness_peak_and_tail_any_tick() {
    // Completeness: every sample-count regime keeps (a) the global peak in
    // SOME bin (max-of-span, never average), (b) the newest sample on the
    // right edge, (c) 1:1 identity while history fits the width.
    use lsmtop::hist::squish_frame;
    for tick_ms in [500u64, 1000, 2000, 5000] {
        for n in [0usize, 1, 59, 60, 61, 200, 600] {
            let mut raw: Vec<f64> = (0..n).map(|i| ((i * 37) % 83) as f64 + 1.0).collect();
            if let Some(p) = [n / 4, n / 2, n.saturating_sub(3)].into_iter().min() {
                if p < n {
                    raw[p] = 1000.0;
                }
            }
            let f = squish_frame(&raw, 60, tick_ms);
            assert_eq!(f.len(), 60);
            if n > 0 {
                assert!(f.iter().any(|&v| (v - 1000.0).abs() < 1e-9), "peak lost n={n} tick={tick_ms}");
                let last = *raw.last().unwrap();
                if n > 60 {
                    // v3: the newest sample IS the rightmost column
                    assert_eq!(f[59], last, "tail dropped n={n} tick={tick_ms}");
                }
            }
            // 1:1 identity: short history rides the RIGHT edge untouched
            if n > 0 && n <= 60 {
                assert_eq!(&f[60 - n..], &raw[..], "identity broken n={n} tick={tick_ms}");
            }
        }
    }
}

#[test]
fn squish_v3_near_field_is_true_1to1() {
    // The right K columns are the ordinary scroll view: exact identity
    // with the newest K samples, at every tick (maintainer: "the first third
    // should look almost like a normal graph on the right").
    use lsmtop::hist::{squish_frame, squish_near_field};
    assert_eq!(squish_near_field(60, 0), 15); // 30 s @ 2000 ms
    assert_eq!(squish_near_field(60, 500), 20); // capped by a third of the width
    assert_eq!(squish_near_field(60, 5000), 6);
    assert_eq!(squish_near_field(60, 12000), 4); // floor 4 keeps a live edge
    for tick_ms in [500u64, 1000, 2000, 5000] {
        let k = squish_near_field(60, tick_ms);
        for n in [61usize, 200, 600, 5000] {
            let raw: Vec<f64> = (0..n).map(|i| ((i * 37) % 83) as f64 + 1.0).collect();
            let f = squish_frame(&raw, 60, tick_ms);
            assert_eq!(&f[60 - k..], &raw[n - k..], "near field not 1:1 n={n} tick={tick_ms}");
        }
    }
}

#[test]
fn squish_v3_updates_land_immediately() {
    // The B6 delay, once and for all: every pushed sample shows up on the
    // right edge on the SAME scrape, and the 1:1 tail walks left with no
    // quantum stall anywhere in the fill.
    use lsmtop::hist::squish_frame;
    let mut raw: Vec<f64> = (0..70).map(|i| (i % 17) as f64).collect();
    for i in 70..260u32 {
        raw.push(i as f64);
        let f = squish_frame(&raw, 60, 2000);
        assert_eq!(f[59], i as f64, "update #i missed the display");
    }
}

#[test]
fn squish_v3_tightening_is_monotone_leftward() {
    // Compression must only INCREASE away from the right edge: a column
    // further left never covers FEWER samples than one to its right.
    // Probed structurally via distinct ramps: a monotone-increasing input
    // must stay non-decreasing across the frame (max-of-bin + nested
    // edges), and a single spike at position p must ride exactly ONE
    // column for every n x tick combo (bins tile, never overlap).
    use lsmtop::hist::squish_frame;
    for tick_ms in [500u64, 2000, 5000] {
        for n in [61usize, 120, 400, 600] {
            let mono: Vec<f64> = (0..n).map(|i| i as f64).collect();
            let f = squish_frame(&mono, 60, tick_ms);
            for w in f.windows(2) {
                assert!(w[0] <= w[1] + 1e-9, "warp non-monotone n={n} tick={tick_ms}");
            }
            let spike = (0..n).map(|_| 0.0).collect::<Vec<f64>>();
            for p in [0usize, n / 3, n / 2, n - 21, n - 1] {
                if p >= n { continue; }
                let mut s2 = spike.clone();
                s2[p] = 7.0;
                let f2 = squish_frame(&s2, 60, tick_ms);
                let hits = f2.iter().filter(|&&v| v == 7.0).count();
                assert_eq!(hits, 1, "spike at {p} n={n} tick={tick_ms}: {hits} columns");
            }
        }
    }
}

#[test]
fn squish_v4_centre_is_half_time() {
    // Feed a ramp (value == sample index): each frame column then shows
    // the LAST index of its bin, so frame differences read back the bin
    // widths directly. Law: near field width 1; the visual CENTRE of the
    // pane ~2 samples/column (half time); left edge capped near 5 with
    // monotone growth; overflow is absorbed by the oldest column only.
    use lsmtop::hist::{squish_frame, squish_near_field};
    for (w, n) in [(60usize, 400usize), (60, 1200), (120, 3000), (40, 200)] {
        let raw: Vec<f64> = (0..n).map(|i| i as f64).collect();
        let f = squish_frame(&raw, w, 2000);
        let k = squish_near_field(w, 2000).min(n - 1);
        // near field: 1:1
        for x in (w - k)..w {
            assert_eq!(f[x] - f[x - 1], 1.0, "w={w} n={n} near field x={x}");
        }
        // widths monotone leftward, centre ~2, edge <= 5 while under cap
        let mut widths: Vec<f64> = (1..(w - k)).map(|x| f[x] - f[x - 1]).collect();
        widths.reverse(); // index 0 = just left of near field
        // integer edges make widths step-like: the LAW is monotone, the
        // rounding jitters at most one sample between neighbours
        for pair in widths.windows(2) {
            assert!(pair[1] >= pair[0] - 1.01, "w={w} n={n}: widths not monotone leftward {widths:?}");
        }
        let mid = widths.len() / 2;
        // under the cap (n small enough to tile) the centre hugs 2.0;
        // when overflow kicks in the LEFTMOST column absorbs, centre stays
        let centre = widths[mid];
        assert!((1.5..=2.6).contains(&centre), "w={w} n={n}: centre width {centre}");
        let interior_max = widths[1..].iter().cloned().fold(f64::MIN, f64::max);
        assert!(interior_max <= 5.5, "w={w} n={n}: interior width {interior_max} past cap");
    }
}
