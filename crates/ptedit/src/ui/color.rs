//! Color conversions for the color picker: linear RGB (what scenes store) to and from the
//! sRGB encoding the screen shows, HSV over the sRGB values, and black-body colors.

/// Linear to sRGB-encoded, per component (values above 1 stay above 1).
pub fn to_srgb(c: f64) -> f64 {
    if c <= 0.003_130_8 { 12.92 * c.max(0.0) } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }
}

pub fn from_srgb(c: f64) -> f64 {
    if c <= 0.040_45 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

/// A linear color as a 0x00RRGGBB pixel (clamped).
pub fn pixel(rgb: [f64; 3]) -> u32 {
    let b = |c: f64| (to_srgb(c).clamp(0.0, 1.0) * 255.0 + 0.5) as u32;
    (b(rgb[0]) << 16) | (b(rgb[1]) << 8) | b(rgb[2])
}

/// RGB in 0-1 to hue (0-1), saturation, value.
#[expect(clippy::float_cmp, reason = "max is one of the components exactly")]
pub fn rgb_to_hsv(c: [f64; 3]) -> [f64; 3] {
    let max = c[0].max(c[1]).max(c[2]);
    let min = c[0].min(c[1]).min(c[2]);
    let d = max - min;
    let h = if d <= 0.0 {
        0.0
    } else if max == c[0] {
        ((c[1] - c[2]) / d).rem_euclid(6.0) / 6.0
    } else if max == c[1] {
        ((c[2] - c[0]) / d + 2.0) / 6.0
    } else {
        ((c[0] - c[1]) / d + 4.0) / 6.0
    };
    [h, if max > 0.0 { d / max } else { 0.0 }, max]
}

pub fn hsv_to_rgb(hsv: [f64; 3]) -> [f64; 3] {
    let [h, s, v] = hsv;
    let h = h.rem_euclid(1.0) * 6.0;
    let f = h - h.floor();
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - s * f), v * (1.0 - s * (1.0 - f)));
    match h as u32 {
        0 => [v, t, p],
        1 => [q, v, p],
        2 => [p, v, t],
        3 => [p, q, v],
        4 => [t, p, v],
        _ => [v, p, q],
    }
}

/// The linear color of a black body at `kelvin`, scaled so its largest component is 1.
pub fn kelvin(kelvin: f64) -> [f64; 3] {
    let c = pt_core::spectrum::blackbody_rgb(kelvin as f32);
    let v = [f64::from(c.x).max(0.0), f64::from(c.y).max(0.0), f64::from(c.z).max(0.0)];
    let m = v[0].max(v[1]).max(v[2]).max(1e-9);
    v.map(|x| x / m)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversions_round_trip() {
        for c in [[0.2, 0.5, 0.9], [1.0, 0.0, 0.0], [0.3, 0.3, 0.3], [0.0, 0.7, 0.1]] {
            let back = hsv_to_rgb(rgb_to_hsv(c));
            assert!((0..3).all(|i| (back[i] - c[i]).abs() < 1e-9), "{c:?} -> {back:?}");
        }
        for x in [0.0, 0.002, 0.18, 0.5, 1.0] {
            assert!((from_srgb(to_srgb(x)) - x).abs() < 1e-9);
        }
        assert_eq!(pixel([1.0, 0.0, 0.5]), 0xFF_00_BC);
        let warm = kelvin(2700.0);
        let cool = kelvin(9000.0);
        assert!(warm[0] > warm[2] && cool[2] > cool[0], "{warm:?} {cool:?}");
    }
}
