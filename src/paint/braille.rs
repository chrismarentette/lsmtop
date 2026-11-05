pub fn encode(values: &[f64], w: u16, h: u16) -> Vec<String> {
    let mut out = Vec::with_capacity(h as usize);
    if w == 0 || h == 0 || values.is_empty() {
        return out;
    }
    
    let w_usize = w as usize;
    let h_usize = h as usize;
    let h_dots = h_usize * 4;
    
    let mut dh = Vec::with_capacity(w_usize * 2);
    let last = values.len().saturating_sub(1) as f64;
    for px in 0..(w_usize * 2) {
        let smp = (px as f64 / 2.0 - 0.5).clamp(0.0, last);
        let i0 = smp.trunc() as usize;
        let i1 = (i0 + 1).min(values.len() - 1);
        let fr = smp - i0 as f64;
        let v = values[i0] + (values[i1] - values[i0]) * fr;
        let clamped = v.clamp(0.0, 1.0);
        dh.push(clamped * (h_dots as f64 - 0.01));
    }
    
    let dot_l = [0x01, 0x02, 0x04, 0x40];
    let dot_r = [0x08, 0x10, 0x20, 0x80];
    
    for y in 0..h_usize {
        let base = (h_usize - 1 - y) * 4;
        let mut row = String::with_capacity(w_usize);
        let mut prev_h = dh[0];
        
        for x in 0..w_usize {
            let left_h = dh[2 * x];
            let right_h = dh[2 * x + 1];
            
            let mut pat = 0u32;
            for r in 0..4 {
                let a = base + (3 - r);
                
                let (lo_l, hi_l) = if prev_h < left_h { (prev_h, left_h) } else { (left_h, prev_h) };
                let (lo_r, hi_r) = if left_h < right_h { (left_h, right_h) } else { (right_h, left_h) };
                
                let lit_l = lo_l as usize <= a && a <= hi_l as usize;
                let lit_r = lo_r as usize <= a && a <= hi_r as usize;
                
                if lit_l { pat |= dot_l[r]; }
                if lit_r { pat |= dot_r[r]; }
            }
            
            prev_h = right_h;
            
            if pat == 0 {
                row.push(' ');
            } else {
                row.push(std::char::from_u32(0x2800 + pat).unwrap());
            }
        }
        out.push(row);
    }
    
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_braille_flat_half() {
        // flat 0.5 line, h=1.
        let strings = encode(&[0.5, 0.5], 2, 1);
        assert_eq!(strings, vec!["\u{2824}\u{2824}"]);
    }

    #[test]
    fn test_braille_single_step() {
        // single-step edge [0.0, 1.0], w=2, h=1
        let strings = encode(&[0.0, 1.0], 2, 1);
        assert_eq!(strings, vec!["\u{28c0}\u{287c}"]);
    }
    
    #[test]
    fn test_braille_full_col() {
        // full-height column [0.0, 1.0, 0.0], w=3, h=1
        let strings = encode(&[0.0, 1.0, 0.0], 3, 1);
        // let's derive it:
        // px=0: smp=0, v=0 -> dh=0
        // px=1: smp=0, v=0 -> dh=0
        // px=2: smp=0.5, v=0.5 -> dh=1.995
        // px=3: smp=1.0, v=1.0 -> dh=3.99
        // px=4: smp=1.5, v=0.5 -> dh=1.995
        // px=5: smp=2.0, v=0.0 -> dh=0
        // cell 0: prev_h=0, left_h=0, right_h=0 -> pat=0xC0 (⣀)
        // cell 1: prev_h=0, left_h=1.995, right_h=3.99
        //    L: 0 to 1.995 (a=0,1) -> 0x40 | 0x04
        //    R: 1.995 to 3.99 (a=1,2,3) -> 0x20 | 0x10 | 0x08
        //    pat = 0x7C (⡼)
        // cell 2: prev_h=3.99, left_h=1.995, right_h=0
        //    L: 1.995 to 3.99 (a=1,2,3) -> 0x04 | 0x02 | 0x01
        //    R: 0 to 1.995 (a=0,1) -> 0x80 | 0x20
        //    pat = 0xA7 (⢧)
        assert_eq!(strings, vec!["\u{28c0}\u{287c}\u{28a7}"]);
    }
}
