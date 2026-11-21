//! Tray icons, drawn in code: a microphone in white (idle), red (recording) or amber (busy).

use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateDIBSection, DIB_RGB_COLORS,
    DeleteObject,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateIconIndirect, GetSystemMetrics, HICON, ICONINFO, SM_CXSMICON,
};
use windows::core::Result;

use crate::icon_shape::coverage;

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
