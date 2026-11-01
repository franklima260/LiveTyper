//! Tray icons, drawn in code: a microphone in white (idle), red (recording) or amber (busy).

use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateDIBSection, DIB_RGB_COLORS,
    DeleteObject,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateIconIndirect, GetSystemMetrics, HICON, ICONINFO, SM_CXSMICON,
};
use windows::core::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconState {
    Idle,
    Recording,
    Busy,
}

pub struct Icons {
    pub idle: HICON,
    pub recording: HICON,
    pub busy: HICON,
}

impl Icons {
    pub fn new() -> Result<Icons> {
        let size = unsafe { GetSystemMetrics(SM_CXSMICON) }.max(16) as usize;
        Ok(Icons {
            idle: make_icon(size, [245, 245, 245])?,
            recording: make_icon(size, [229, 72, 77])?,
            busy: make_icon(size, [245, 165, 36])?,
        })
    }

    pub fn get(&self, state: IconState) -> HICON {
        match state {
            IconState::Idle => self.idle,
            IconState::Recording => self.recording,
            IconState::Busy => self.busy,
        }
    }
}

/// Coverage 0..1 of the microphone shape at a point, in a 32×32 design grid.
fn coverage(x: f32, y: f32) -> f32 {
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

fn make_icon(size: usize, rgb: [u8; 3]) -> Result<HICON> {
    unsafe {
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: size as i32,
                biHeight: -(size as i32), // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let color = CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0)?;
        let pixels = std::slice::from_raw_parts_mut(bits as *mut u8, size * size * 4);
        let grid = 32.0 / size as f32;
        for py in 0..size {
            for px in 0..size {
                // 4×4 supersampling for smooth edges.
                let mut sum = 0.0;
                for sy in 0..4 {
                    for sx in 0..4 {
                        let x = (px as f32 + (sx as f32 + 0.5) / 4.0) * grid;
                        let y = (py as f32 + (sy as f32 + 0.5) / 4.0) * grid;
                        sum += coverage(x, y);
                    }
                }
                let alpha = sum / 16.0;
                let i = (py * size + px) * 4;
                // Premultiplied BGRA.
                pixels[i] = (rgb[2] as f32 * alpha) as u8;
                pixels[i + 1] = (rgb[1] as f32 * alpha) as u8;
                pixels[i + 2] = (rgb[0] as f32 * alpha) as u8;
                pixels[i + 3] = (alpha * 255.0) as u8;
            }
        }
        let mask = CreateBitmap(size as i32, size as i32, 1, 1, None);
        let icon = CreateIconIndirect(&ICONINFO {
            fIcon: true.into(),
            hbmMask: mask,
            hbmColor: color,
            ..Default::default()
        });
        let _ = DeleteObject(color.into());
        let _ = DeleteObject(mask.into());
        icon
    }
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
}
