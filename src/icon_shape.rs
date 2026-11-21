//! The tray icon shape, a microphone, shared by the Windows and Linux tray icons.

/// Coverage 0..1 of the microphone shape at a point, in a 32×32 design grid.
pub fn coverage(x: f32, y: f32) -> f32 {
    let inside = |d: f32| (0.5 - d).clamp(0.0, 1.0);
    // Capsule body: center line x = 16, y 7..15, radius 5.
    let cy = y.clamp(7.0, 15.0);
    let body = ((x - 16.0).powi(2) + (y - cy).powi(2)).sqrt() - 5.0;
    // Lower half ring around the body: center (16, 14), radius 9, 2 thick.
    let ring = if y >= 14.0 {
        (((x - 16.0).powi(2) + (y - 14.0).powi(2)).sqrt() - 9.0).abs() - 1.1
    } else {
        f32::MAX
    };
    // Stem and base.
    let stem = ((x - 16.0).abs() - 1.1).max((y - 25.5).abs() - 2.5);
    let base = ((x - 16.0).abs() - 5.5).max((y - 28.0).abs() - 1.1);
    inside(body)
        .max(inside(ring))
        .max(inside(stem))
        .max(inside(base))
}

/// The alpha of each pixel of a `size`×`size` icon, row by row, with 4×4 supersampling.
#[cfg_attr(windows, allow(dead_code))]
pub fn alpha(size: usize) -> Vec<f32> {
    let grid = 32.0 / size as f32;
    let mut out = Vec::with_capacity(size * size);
    for py in 0..size {
        for px in 0..size {
            let mut sum = 0.0;
            for sy in 0..4 {
                for sx in 0..4 {
                    let x = (px as f32 + (sx as f32 + 0.5) / 4.0) * grid;
                    let y = (py as f32 + (sy as f32 + 0.5) / 4.0) * grid;
                    sum += coverage(x, y);
                }
            }
            out.push(sum / 16.0);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shape_covers_body_and_leaves_corners_empty() {
        assert_eq!(coverage(16.0, 10.0), 1.0);
        assert_eq!(coverage(16.0, 28.0), 1.0);
        assert_eq!(coverage(1.0, 1.0), 0.0);
        assert_eq!(coverage(31.0, 31.0), 0.0);
    }

    #[test]
    fn alpha_has_one_value_per_pixel() {
        let a = alpha(22);
        assert_eq!(a.len(), 22 * 22);
        assert_eq!(a[0], 0.0);
        assert!(a.contains(&1.0));
    }
}
