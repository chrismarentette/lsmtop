use lsmtop::hist::Ring;

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
    
    
    for tick in 0..=50 {
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
    let frame = lsmtop::hist::squish_frame(&mono, 60, 0);
    for w in frame.windows(2) {
        assert!(w[0] <= w[1] + 1e-9, "not monotone: {} > {}", w[0], w[1]);
    }
    
    // all-zero
    let zeros = vec![0.0; 200];
    let frame = lsmtop::hist::squish_frame(&zeros, 60, 0);
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

fn lcg(seed: u64, count: usize) -> Vec<f64> {
    let mut val = seed;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        val = val.wrapping_mul(1103515245).wrapping_add(12345) & 0x7FFFFFFF;
        out.push(val as f64 / 0x7FFFFFFF as f64 * 100.0);
    }
    out
}

#[test]
fn squish_golden_parity() {
    let golden_json = std::fs::read_to_string("tests/fixtures/squish_golden.json").unwrap();
    let golden: std::collections::HashMap<String, Vec<f64>> = serde_json::from_str(&golden_json).unwrap();
    
    let raw = lcg(42, 200);
    for tick in 0..=30 {
        let rust_frame = lsmtop::hist::squish_frame(&raw, 60, tick);
        let py_frame = golden.get(&tick.to_string()).unwrap();
        
        assert_eq!(rust_frame.len(), py_frame.len());
        for (i, (r, p)) in rust_frame.iter().zip(py_frame.iter()).enumerate() {
            assert!((r - p).abs() < 1e-9, "Mismatch at tick {} idx {}: rust {} != py {}", tick, i, r, p);
        }
    }
}
