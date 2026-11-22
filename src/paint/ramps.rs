use ratatui::style::Color;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ramp {
    Heat,
    Spectrum,
    Ice,
}

impl Ramp {
    pub fn from_name(name: &str) -> Self {
        match name {
            "spectrum" => Ramp::Spectrum,
            "ice" => Ramp::Ice,
            _ => Ramp::Heat,
        }
    }
}

const RAMP_HEAT: &[(f64, (u8, u8, u8))] = &[
    (0.00, (57, 255, 148)),
    (0.14, (120, 226, 110)),
    (0.35, (198, 226, 88)),
    (0.58, (255, 196, 76)),
    (0.80, (255, 124, 60)),
    (1.00, (224, 60, 60)),
];

const RAMP_SPECTRUM: &[(f64, (u8, u8, u8))] = &[
    (0.00, (0, 175, 95)),
    (0.16, (95, 215, 135)),
    (0.32, (0, 215, 255)),
    (0.50, (95, 135, 255)),
    (0.66, (175, 95, 215)),
    (0.82, (255, 95, 175)),
    (0.92, (255, 95, 95)),
    (1.00, (255, 184, 108)),
];

const RAMP_ICE: &[(f64, (u8, u8, u8))] = &[
    (0.00, (16, 32, 110)),
    (0.35, (40, 110, 235)),
    (0.70, (80, 215, 255)),
    (1.00, (235, 250, 255)),
];

pub fn ramp_color(ramp: Ramp, t: f64) -> Color {
    let stops = match ramp {
        Ramp::Heat => RAMP_HEAT,
        Ramp::Spectrum => RAMP_SPECTRUM,
        Ramp::Ice => RAMP_ICE,
    };
    
    let f = t.clamp(0.0, 1.0);
    
    for i in 1..stops.len() {
        let (f1, c1) = stops[i];
        if f <= f1 {
            let (f0, c0) = stops[i - 1];
            let u = if f1 > f0 { (f - f0) / (f1 - f0) } else { 0.0 };
            
            let r = (c0.0 as f64 + (c1.0 as f64 - c0.0 as f64) * u).round() as u8;
            let g = (c0.1 as f64 + (c1.1 as f64 - c0.1 as f64) * u).round() as u8;
            let b = (c0.2 as f64 + (c1.2 as f64 - c0.2 as f64) * u).round() as u8;
            
            return Color::Rgb(r, g, b);
        }
    }
    
    let last = stops.last().unwrap().1;
    Color::Rgb(last.0, last.1, last.2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ramp_endpoints() {
        // test heat endpoints (direction explicitly green -> red as per v9.11)
        assert_eq!(ramp_color(Ramp::Heat, 0.0), Color::Rgb(57, 255, 148)); // Green floor
        assert_eq!(ramp_color(Ramp::Heat, 1.0), Color::Rgb(224, 60, 60));  // Red peak
        
        assert_eq!(ramp_color(Ramp::Spectrum, 0.0), Color::Rgb(0, 175, 95));
        assert_eq!(ramp_color(Ramp::Spectrum, 1.0), Color::Rgb(255, 184, 108));
        
        assert_eq!(ramp_color(Ramp::Ice, 0.0), Color::Rgb(16, 32, 110));
        assert_eq!(ramp_color(Ramp::Ice, 1.0), Color::Rgb(235, 250, 255));
    }

    #[test]
    fn test_ramp_midpoint() {
        // heat midpoint approx 0.07 between 0.00 and 0.14
        let c = ramp_color(Ramp::Heat, 0.07);
        if let Color::Rgb(r, g, b) = c {
            assert!((r as i32 - 88).abs() <= 1);
            assert!((g as i32 - 240).abs() <= 1);
            assert!((b as i32 - 129).abs() <= 1);
        } else {
            panic!("Expected Rgb");
        }
    }

    #[test]
    fn test_unknown_ramp() {
        assert_eq!(Ramp::from_name("unknown"), Ramp::Heat);
    }
}