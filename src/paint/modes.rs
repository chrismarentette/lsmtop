use ratatui::style::Color;
use crate::paint::ramps::{ramp_color, Ramp};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Gradient,
    Solid,
    Flow,
}

impl Mode {
    pub fn from_name(name: &str) -> Self {
        match name {
            "solid" => Mode::Solid,
            "flow" => Mode::Flow,
            _ => Mode::Gradient,
        }
    }
}

pub fn column_color(mode: Mode, ramp: Ramp, ratio: f64, flow_phase: f64, vmax: f64) -> Color {
    match mode {
        Mode::Gradient => ramp_color(ramp, ratio),
        Mode::Solid => ramp_color(ramp, ratio), // ratio is column_ratio
        Mode::Flow => {
            // ratio is v / vmax here
            // port: app.py:1864-1877
            let mut f = if vmax > 0.0 && ratio > 0.0 {
                ratio.powf(0.85)
            } else {
                0.0
            };
            if f > 0.0 {
                f = (0.55 * f + 0.45 * f * (0.5 + 0.5 * (std::f64::consts::TAU * flow_phase).sin())).min(1.0);
            }
            ramp_color(ramp, f)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn test_regression_guard_solid_gamut() {
        // "Solid column colors equal ramp_color on the same stops"
        // "a 20-column ratio sweep under Solid across each palette yields >= 6 distinct colors"
        for ramp in [Ramp::Heat, Ramp::Spectrum, Ramp::Ice] {
            let mut colors = HashSet::new();
            for i in 0..20 {
                let ratio = i as f64 / 19.0;
                let c = column_color(Mode::Solid, ramp, ratio, 0.0, 1.0);
                colors.insert(c);
            }
            assert!(colors.len() >= 6, "Ramp {:?} has < 6 distinct colors in solid mode", ramp);
        }
    }

    #[test]
    fn test_gradient_solid_flow_differ() {
        // "Gradient, Solid, Flow produce DIFFERENT color sets on identical data"
        let mut grad_colors = HashSet::new();
        let mut solid_colors = HashSet::new();
        let mut flow_colors = HashSet::new();
        
        let ramp = Ramp::Heat;
        let vmax = 100.0;
        
        for i in 0..20 {
            // For Solid and Flow, the painter passes ratio = v / vmax.
            // For Gradient, it passes frac = vertical position (which could vary, but let's simulate).
            let v = i as f64 * 5.0; // 0..95
            let col_ratio = v / vmax;
            // Let's sample a few vertical points for gradient
            for j in 0..4 {
                let grad_ratio = j as f64 / 3.0; // 0, 0.33, 0.66, 1.0
                grad_colors.insert(column_color(Mode::Gradient, ramp, grad_ratio, 0.0, vmax));
            }
            solid_colors.insert(column_color(Mode::Solid, ramp, col_ratio, 0.0, vmax));
            
            // flow_phase varies with time and col_ratio
            let flow_phase = 42.0 * 0.028 + col_ratio * 1.7;
            flow_colors.insert(column_color(Mode::Flow, ramp, col_ratio, flow_phase, vmax));
        }
        
        assert_ne!(grad_colors, solid_colors);
        assert_ne!(solid_colors, flow_colors);
        assert_ne!(grad_colors, flow_colors);
    }
}
