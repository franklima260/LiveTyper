//! The Scratch Pad: a read-only window that shows the Draft during a Dictation.
//!
//! It never takes the focus and lets mouse clicks pass through (`WS_EX_NOACTIVATE`,
//! `WS_EX_TRANSPARENT`), so the caret stays in the Target Text Box. It is drawn with Direct2D and
//! DirectWrite. It starts with one line of text and grows upward to at most 10 lines.

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_U, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_ELLIPSE,
    D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_HWND_RENDER_TARGET_PROPERTIES,
    D2D1_PRESENT_OPTIONS_NONE, D2D1_RENDER_TARGET_PROPERTIES, D2D1_ROUNDED_RECT, D2D1CreateFactory,
    ID2D1Factory, ID2D1HwndRenderTarget, ID2D1SolidColorBrush,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_METRICS,
    DWRITE_TEXT_RANGE, DWRITE_WORD_WRAPPING_NO_WRAP, DWRITE_WORD_WRAPPING_WRAP,
    DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat, IDWriteTextLayout,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, PAINTSTRUCT, ValidateRect};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, HTTRANSPARENT, HWND_TOPMOST, LWA_ALPHA, MA_NOACTIVATE,
    RegisterClassW, SPI_GETWORKAREA, SW_HIDE, SWP_NOACTIVATE, SWP_SHOWWINDOW,
    SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SetLayeredWindowAttributes, SetWindowPos, ShowWindow,
    SystemParametersInfoW, WM_MOUSEACTIVATE, WM_NCHITTEST, WM_PAINT, WNDCLASSW, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};
use windows::core::{Interface, Result, w};

use crate::draft::DraftView;

// Sizes in DIPs (1/96 inch).
const PADDING: f32 = 14.0;
const STATUS_HEIGHT: f32 = 18.0;
const GAP: f32 = 8.0;
const TEXT_SIZE: f32 = 18.0;
const STATUS_SIZE: f32 = 12.5;
const MAX_LINES: u32 = 10;
const LEVEL_WIDTH: f32 = 90.0;
const CORNER: f32 = 10.0;
/// Distance from the bottom of the work area when no position is set.
const DEFAULT_BOTTOM_MARGIN: f32 = 24.0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Listening,
    Loading,
    Finishing,
    Message(String),
}

struct Resources {
    target: ID2D1HwndRenderTarget,
    background: ID2D1SolidColorBrush,
    text: ID2D1SolidColorBrush,
    unchecked: ID2D1SolidColorBrush,
    muted: ID2D1SolidColorBrush,
    recording: ID2D1SolidColorBrush,
    waiting: ID2D1SolidColorBrush,
    track: ID2D1SolidColorBrush,
}

pub struct ScratchPad {
    hwnd: HWND,
    d2d: ID2D1Factory,
    dwrite: IDWriteFactory,
    text_format: IDWriteTextFormat,
    status_format: IDWriteTextFormat,
    resources: Option<Resources>,
    anchor: Option<(i32, i32)>,
    width_dip: f32,
    status: Status,
    view: DraftView,
    level: f32,
    visible: bool,
    /// Current size in pixels.
    size: (i32, i32),
}

impl ScratchPad {
    /// `anchor` is the center of the bottom edge in screen pixels; `None` = bottom center of the main screen.
    pub fn new(anchor: Option<(i32, i32)>, width_dip: u32) -> Result<ScratchPad> {
        unsafe {
            let instance = GetModuleHandleW(None)?;
            let class_name = w!("LiveTyperScratchPad");
            let class = WNDCLASSW {
                lpfnWndProc: Some(window_proc),
                hInstance: instance.into(),
                lpszClassName: class_name,
                ..Default::default()
            };
            RegisterClassW(&class);
            let hwnd = CreateWindowExW(
                WS_EX_NOACTIVATE
                    | WS_EX_TOPMOST
                    | WS_EX_TOOLWINDOW
                    | WS_EX_TRANSPARENT
                    | WS_EX_LAYERED,
                class_name,
                w!("LiveTyper Scratch Pad"),
                WS_POPUP,
                0,
                0,
                1,
                1,
                None,
                None,
                Some(instance.into()),
                None,
            )?;
            SetLayeredWindowAttributes(hwnd, COLORREF(0), 245, LWA_ALPHA)?;

            let d2d: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let text_format = dwrite.CreateTextFormat(
                w!("Segoe UI"),
                None,
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                TEXT_SIZE,
                w!("en-us"),
            )?;
            text_format.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP)?;
            let status_format = dwrite.CreateTextFormat(
                w!("Segoe UI"),
                None,
                DWRITE_FONT_WEIGHT_SEMI_BOLD,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                STATUS_SIZE,
                w!("en-us"),
            )?;
            status_format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;

            Ok(ScratchPad {
                hwnd,
                d2d,
                dwrite,
                text_format,
                status_format,
                resources: None,
                anchor,
                width_dip: width_dip.max(240) as f32,
                status: Status::Listening,
                view: DraftView::default(),
                level: 0.0,
                visible: false,
                size: (0, 0),
            })
        }
    }

    /// Shows the Scratch Pad with an empty Draft, without taking the focus.
    pub fn show(&mut self, status: Status) {
        self.status = status;
        self.view = DraftView::default();
        self.level = 0.0;
        self.visible = true;
        self.layout();
    }

    pub fn hide(&mut self) {
        self.visible = false;
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_HIDE);
        }
    }

    pub fn set_status(&mut self, status: Status) {
        if self.status != status {
            self.status = status;
            self.layout();
        }
    }

    pub fn set_draft(&mut self, view: DraftView) {
        if self.view != view {
            self.view = view;
            self.layout();
        }
    }

    pub fn set_level(&mut self, level: f32) {
        if (self.level - level).abs() > 0.01 {
            self.level = level;
            self.invalidate();
        }
    }

    fn scale(&self) -> f32 {
        let dpi = unsafe { GetDpiForWindow(self.hwnd) };
        if dpi == 0 { 1.0 } else { dpi as f32 / 96.0 }
    }

    fn text_width(&self) -> f32 {
        self.width_dip - 2.0 * PADDING
    }

    fn text_layout(&self) -> Result<IDWriteTextLayout> {
        let text = self.view.full();
        let wide: Vec<u16> = text.encode_utf16().collect();
        unsafe {
            self.dwrite
                .CreateTextLayout(&wide, &self.text_format, self.text_width(), f32::MAX)
        }
    }

    fn line_metrics(&self) -> (u32, f32) {
        let mut metrics = DWRITE_TEXT_METRICS::default();
        let ok = self
            .text_layout()
            .and_then(|layout| unsafe { layout.GetMetrics(&mut metrics) })
            .is_ok();
        let lines = if ok { metrics.lineCount.max(1) } else { 1 };
        let line_height = if ok && metrics.lineCount > 0 {
            metrics.height / metrics.lineCount as f32
        } else {
            TEXT_SIZE * 1.33
        };
        (lines, line_height)
    }

    /// Sets the window size for the current text, anchored at the bottom center, and redraws.
    fn layout(&mut self) {
        if !self.visible {
            return;
        }
        let scale = self.scale();
        let (lines, line_height) = self.line_metrics();
        let height_dip =
            2.0 * PADDING + STATUS_HEIGHT + GAP + lines.min(MAX_LINES) as f32 * line_height;
        let width = (self.width_dip * scale).round() as i32;
        let height = (height_dip * scale).round() as i32;
        let (cx, bottom) = self.anchor.unwrap_or_else(|| default_anchor(scale));
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                cx - width / 2,
                bottom - height,
                width,
                height,
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            );
        }
        if self.size != (width, height) {
            self.size = (width, height);
            if let Some(r) = &self.resources {
                unsafe {
                    let _ = r.target.Resize(&D2D_SIZE_U {
                        width: width as u32,
                        height: height as u32,
                    });
                }
            }
        }
        self.invalidate();
    }

    fn invalidate(&self) {
        if self.visible {
            unsafe {
                let _ = windows::Win32::Graphics::Gdi::InvalidateRect(Some(self.hwnd), None, false);
            }
        }
    }

    fn create_resources(&self) -> Result<Resources> {
        unsafe {
            let dpi = GetDpiForWindow(self.hwnd).max(96) as f32;
            let target = self.d2d.CreateHwndRenderTarget(
                &D2D1_RENDER_TARGET_PROPERTIES {
                    pixelFormat: D2D1_PIXEL_FORMAT {
                        format: DXGI_FORMAT_B8G8R8A8_UNORM,
                        alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                    },
                    dpiX: dpi,
                    dpiY: dpi,
                    ..Default::default()
                },
                &D2D1_HWND_RENDER_TARGET_PROPERTIES {
                    hwnd: self.hwnd,
                    pixelSize: D2D_SIZE_U {
                        width: self.size.0.max(1) as u32,
                        height: self.size.1.max(1) as u32,
                    },
                    presentOptions: D2D1_PRESENT_OPTIONS_NONE,
                },
            )?;
            let brush =
                |r: f32, g: f32, b: f32| target.CreateSolidColorBrush(&color(r, g, b), None);
            Ok(Resources {
                background: brush(0.12, 0.12, 0.14)?,
                text: brush(0.95, 0.95, 0.95)?,
                unchecked: brush(0.55, 0.55, 0.58)?,
                muted: brush(0.70, 0.70, 0.72)?,
                recording: brush(0.90, 0.28, 0.30)?,
                waiting: brush(0.96, 0.65, 0.14)?,
                track: brush(0.25, 0.25, 0.28)?,
                target,
            })
        }
    }

    /// Draws the Scratch Pad. Called on WM_PAINT.
    pub fn paint(&mut self) {
        if self.resources.is_none() {
            match self.create_resources() {
                Ok(r) => self.resources = Some(r),
                Err(e) => {
                    log::error!("scratch pad: {e}");
                    return;
                }
            }
        }
        if let Err(e) = self.draw() {
            // D2DERR_RECREATE_TARGET and similar: make new resources on the next paint.
            log::info!("scratch pad redraw: {e}");
            self.resources = None;
            self.invalidate();
        }
    }

    fn draw(&self) -> Result<()> {
        let r = self.resources.as_ref().expect("resources exist");
        let scale = self.scale();
        let width = self.size.0 as f32 / scale;
        let height = self.size.1 as f32 / scale;
        unsafe {
            r.target.BeginDraw();
            r.target.Clear(Some(&color(0.12, 0.12, 0.14)));
            r.target.FillRoundedRectangle(
                &D2D1_ROUNDED_RECT {
                    rect: rect(0.0, 0.0, width, height),
                    radiusX: CORNER,
                    radiusY: CORNER,
                },
                &r.background,
            );

            // Status row: a dot, a label, and the microphone level.
            let (dot, label) = match &self.status {
                Status::Listening => (&r.recording, "Listening".to_owned()),
                Status::Loading => (&r.waiting, "Recording · loading models…".to_owned()),
                Status::Finishing => (&r.waiting, "Finishing…".to_owned()),
                Status::Message(text) => (&r.waiting, text.clone()),
            };
            let row_center = PADDING + STATUS_HEIGHT / 2.0;
            r.target.FillEllipse(
                &D2D1_ELLIPSE {
                    point: windows_numerics::Vector2 {
                        X: PADDING + 5.0,
                        Y: row_center,
                    },
                    radiusX: 5.0,
                    radiusY: 5.0,
                },
                dot,
            );
            let wide: Vec<u16> = label.encode_utf16().collect();
            let label_width = width - 2.0 * PADDING - 16.0 - LEVEL_WIDTH - 12.0;
            r.target.DrawText(
                &wide,
                &self.status_format,
                &rect(
                    PADDING + 16.0,
                    PADDING,
                    PADDING + 16.0 + label_width,
                    PADDING + STATUS_HEIGHT,
                ),
                &r.muted,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                windows::Win32::Graphics::DirectWrite::DWRITE_MEASURING_MODE_NATURAL,
            );
            if matches!(self.status, Status::Listening | Status::Loading) {
                let x0 = width - PADDING - LEVEL_WIDTH;
                let bar = |x1: f32| rect(x0, row_center - 2.0, x1, row_center + 2.0);
                r.target.FillRectangle(&bar(x0 + LEVEL_WIDTH), &r.track);
                r.target.FillRectangle(
                    &bar(x0 + LEVEL_WIDTH * self.level.clamp(0.0, 1.0)),
                    &r.recording,
                );
            }

            // The Draft: checked words in white, Unchecked Words in grey. When there are more than
            // 10 lines, the last 10 show.
            let layout = self.text_layout()?;
            let checked_len = self.view.checked.encode_utf16().count() as u32;
            let total_len = self.view.full().encode_utf16().count() as u32;
            if total_len > checked_len {
                layout.SetDrawingEffect(
                    &r.unchecked.cast::<windows::core::IUnknown>()?,
                    DWRITE_TEXT_RANGE {
                        startPosition: checked_len,
                        length: total_len - checked_len,
                    },
                )?;
            }
            let mut metrics = DWRITE_TEXT_METRICS::default();
            layout.GetMetrics(&mut metrics)?;
            let top = PADDING + STATUS_HEIGHT + GAP;
            let visible_height = height - top - PADDING;
            let offset = (metrics.height - visible_height).max(0.0);
            r.target.PushAxisAlignedClip(
                &rect(PADDING, top, width - PADDING, height - PADDING + 2.0),
                D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
            );
            r.target.DrawTextLayout(
                windows_numerics::Vector2 {
                    X: PADDING,
                    Y: top - offset,
                },
                &layout,
                &r.text,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
            );
            r.target.PopAxisAlignedClip();
            r.target.EndDraw(None, None)
        }
    }
}

fn color(r: f32, g: f32, b: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r, g, b, a: 1.0 }
}

fn rect(left: f32, top: f32, right: f32, bottom: f32) -> D2D_RECT_F {
    D2D_RECT_F {
        left,
        top,
        right,
        bottom,
    }
}

/// Bottom center of the main screen's work area (above the taskbar).
fn default_anchor(scale: f32) -> (i32, i32) {
    let mut area = RECT::default();
    unsafe {
        let _ = SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            Some(&mut area as *mut RECT as *mut _),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        );
    }
    (
        (area.left + area.right) / 2,
        area.bottom - (DEFAULT_BOTTOM_MARGIN * scale) as i32,
    )
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            unsafe {
                BeginPaint(hwnd, &mut ps);
            }
            crate::app::paint_scratch_pad();
            unsafe {
                let _ = EndPaint(hwnd, &ps);
                let _ = ValidateRect(Some(hwnd), None);
            }
            LRESULT(0)
        }
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}
