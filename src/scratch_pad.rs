//! The Scratch Pad: a read-only window that shows the Draft during a Dictation.
//!
//! It never takes the focus and lets mouse clicks pass through (`WS_EX_NOACTIVATE`,
//! `WS_EX_TRANSPARENT`), so the caret stays in the Target Text Box. It is drawn with Direct2D and
//! DirectWrite. It starts with one line of text and grows upward to at most 10 lines.
//!
//! The Direct2D and DirectWrite objects use much memory (Direct2D loads the graphics driver), so
//! they are made on the first show, and the render target is released when the Scratch Pad hides.

use windows::Win32::Foundation::{COLORREF, E_FAIL, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_U, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_ELLIPSE,
    D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_HWND_RENDER_TARGET_PROPERTIES,
    D2D1_PRESENT_OPTIONS_NONE, D2D1_RENDER_TARGET_PROPERTIES, D2D1_ROUNDED_RECT, D2D1CreateFactory,
    ID2D1Factory, ID2D1HwndRenderTarget, ID2D1RenderTarget, ID2D1SolidColorBrush,
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
    PostMessageW, RegisterClassW, SPI_GETWORKAREA, SW_HIDE, SWP_NOACTIVATE, SWP_SHOWWINDOW,
    SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SetLayeredWindowAttributes, SetWindowPos, ShowWindow,
    SystemParametersInfoW, WM_APP, WM_MOUSEACTIVATE, WM_NCHITTEST, WM_PAINT, WNDCLASSW,
    WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};
use windows::core::{Error, Interface, Result, w};

use crate::draft::DraftView;
use crate::meter::{PeakMeter, SEGMENTS};

// Sizes in DIPs (1/96 inch).
const PADDING: f32 = 14.0;
const STATUS_HEIGHT: f32 = 18.0;
const GAP: f32 = 8.0;
const TEXT_SIZE: f32 = 18.0;
const STATUS_SIZE: f32 = 12.5;
const MAX_LINES: u32 = 10;
const LEVEL_WIDTH: f32 = 18.0;
const LEVEL_HEIGHT: f32 = 26.0;
const CORNER: f32 = 10.0;
/// Distance from the bottom of the work area when no position is set.
const DEFAULT_BOTTOM_MARGIN: f32 = 24.0;
/// Asks for a new WM_PAINT. A WM_PAINT cannot do this itself, because its update region is
/// validated when it ends.
const WM_REPAINT: u32 = WM_APP + 1;
/// After this number of failed draws in sequence, the Scratch Pad stops to try again until the next change.
const MAX_REDRAW_TRIES: u32 = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Listening,
    Loading,
    Finishing,
    Message(String),
}

struct Resources {
    target: ID2D1HwndRenderTarget,
    brushes: Brushes,
}

/// The brushes of one render target.
struct Brushes {
    background: ID2D1SolidColorBrush,
    text: ID2D1SolidColorBrush,
    unchecked: ID2D1SolidColorBrush,
    muted: ID2D1SolidColorBrush,
    recording: ID2D1SolidColorBrush,
    waiting: ID2D1SolidColorBrush,
    meter: ID2D1SolidColorBrush,
}

impl Brushes {
    fn new(target: &ID2D1RenderTarget) -> Result<Brushes> {
        let brush =
            |r: f32, g: f32, b: f32| unsafe { target.CreateSolidColorBrush(&color(r, g, b), None) };
        Ok(Brushes {
            background: brush(0.12, 0.12, 0.14)?,
            text: brush(0.95, 0.95, 0.95)?,
            unchecked: brush(0.55, 0.55, 0.58)?,
            muted: brush(0.70, 0.70, 0.72)?,
            recording: brush(0.90, 0.28, 0.30)?,
            waiting: brush(0.96, 0.65, 0.14)?,
            meter: brush(0.0, 0.0, 0.0)?,
        })
    }
}

/// The factories and text formats. Made on the first show, then kept.
struct Graphics {
    d2d: ID2D1Factory,
    dwrite: IDWriteFactory,
    text_format: IDWriteTextFormat,
    status_format: IDWriteTextFormat,
}

impl Graphics {
    fn new() -> Result<Graphics> {
        unsafe {
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
            Ok(Graphics {
                d2d,
                dwrite,
                text_format,
                status_format,
            })
        }
    }
}

pub struct ScratchPad {
    hwnd: HWND,
    /// `None` until the first show.
    graphics: Option<Graphics>,
    /// The render target and brushes. `None` while the Scratch Pad is hidden.
    resources: Option<Resources>,
    /// Failed draws in sequence.
    failed_draws: u32,
    anchor: Option<(i32, i32)>,
    width_dip: f32,
    status: Status,
    view: DraftView,
    meter: PeakMeter,
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

            Ok(ScratchPad {
                hwnd,
                graphics: None,
                resources: None,
                failed_draws: 0,
                anchor,
                width_dip: width_dip.max(240) as f32,
                status: Status::Listening,
                view: DraftView::default(),
                meter: PeakMeter::default(),
                visible: false,
                size: (0, 0),
            })
        }
    }

    /// Shows the Scratch Pad with an empty Draft, without taking the focus.
    pub fn show(&mut self, status: Status) {
        if self.graphics.is_none() {
            match Graphics::new() {
                Ok(g) => self.graphics = Some(g),
                Err(e) => {
                    log::error!("scratch pad graphics: {e}");
                    return;
                }
            }
        }
        self.status = status;
        self.view = DraftView::default();
        self.meter = PeakMeter::default();
        self.visible = true;
        self.failed_draws = 0;
        self.layout();
    }

    /// Hides the Scratch Pad and releases the render target, which holds graphics memory.
    pub fn hide(&mut self) {
        self.visible = false;
        self.resources = None;
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
        if self.meter.update(level) {
            self.invalidate();
        }
    }

    /// The current DPI of the window (it changes when the window moves to a different screen).
    fn dpi(&self) -> f32 {
        let dpi = unsafe { GetDpiForWindow(self.hwnd) };
        if dpi == 0 { 96.0 } else { dpi as f32 }
    }

    fn scale(&self) -> f32 {
        self.dpi() / 96.0
    }

    fn graphics(&self) -> Result<&Graphics> {
        self.graphics.as_ref().ok_or_else(|| Error::from(E_FAIL))
    }

    /// Sets the window size for the current text, anchored at the bottom center, and redraws.
    fn layout(&mut self) {
        if !self.visible {
            return;
        }
        let scale = self.scale();
        let height_dip = match self.graphics() {
            Ok(g) => pad_height(g, &self.view, self.width_dip),
            Err(_) => return,
        };
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
        let dpi = self.dpi();
        let g = self.graphics()?;
        unsafe {
            let target = g.d2d.CreateHwndRenderTarget(
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
            Ok(Resources {
                brushes: Brushes::new(&target)?,
                target,
            })
        }
    }

    /// Draws the Scratch Pad. Called on WM_PAINT.
    pub fn paint(&mut self) {
        if !self.visible {
            return;
        }
        if self.resources.is_none() {
            match self.create_resources() {
                Ok(r) => self.resources = Some(r),
                Err(e) => {
                    log::error!("scratch pad: {e}");
                    return;
                }
            }
        }
        match self.draw() {
            Ok(()) => self.failed_draws = 0,
            Err(e) => {
                // D2DERR_RECREATE_TARGET and similar: make new resources and draw again.
                log::info!("scratch pad redraw: {e}");
                self.resources = None;
                if self.failed_draws < MAX_REDRAW_TRIES {
                    self.failed_draws += 1;
                    unsafe {
                        let _ = PostMessageW(Some(self.hwnd), WM_REPAINT, WPARAM(0), LPARAM(0));
                    }
                }
            }
        }
    }

    fn draw(&self) -> Result<()> {
        let r = self.resources.as_ref().expect("resources exist");
        let g = self.graphics()?;
        let dpi = self.dpi();
        let scale = dpi / 96.0;
        let width = self.size.0 as f32 / scale;
        let height = self.size.1 as f32 / scale;
        unsafe {
            // The target keeps the DPI from its creation; the window can be on a different screen now.
            r.target.SetDpi(dpi, dpi);
            r.target.BeginDraw();
            r.target.Clear(Some(&color(0.12, 0.12, 0.14)));
            draw_pad(
                &r.target,
                &r.brushes,
                g,
                &self.status,
                &self.view,
                &self.meter,
                width,
                height,
            )?;
            r.target.EndDraw(None, None)
        }
    }
}

fn text_layout(g: &Graphics, view: &DraftView, width_dip: f32) -> Result<IDWriteTextLayout> {
    let wide: Vec<u16> = view.full().encode_utf16().collect();
    unsafe {
        g.dwrite
            .CreateTextLayout(&wide, &g.text_format, width_dip - 2.0 * PADDING, f32::MAX)
    }
}

/// The height in DIPs of a Scratch Pad that shows `view`: 1 to 10 lines of text.
fn pad_height(g: &Graphics, view: &DraftView, width_dip: f32) -> f32 {
    let mut metrics = DWRITE_TEXT_METRICS::default();
    let ok = text_layout(g, view, width_dip)
        .and_then(|layout| unsafe { layout.GetMetrics(&mut metrics) })
        .is_ok();
    let lines = if ok { metrics.lineCount.max(1) } else { 1 };
    let line_height = if ok && metrics.lineCount > 0 {
        metrics.height / metrics.lineCount as f32
    } else {
        TEXT_SIZE * 1.33
    };
    2.0 * PADDING + STATUS_HEIGHT + GAP + lines.min(MAX_LINES) as f32 * line_height
}

/// Draws the Scratch Pad at (0, 0) with the size `width` × `height` in DIPs, between the
/// BeginDraw and the EndDraw of `target`.
#[allow(clippy::too_many_arguments)]
fn draw_pad(
    target: &ID2D1RenderTarget,
    r: &Brushes,
    g: &Graphics,
    status: &Status,
    view: &DraftView,
    meter: &PeakMeter,
    width: f32,
    height: f32,
) -> Result<()> {
    unsafe {
        target.FillRoundedRectangle(
            &D2D1_ROUNDED_RECT {
                rect: rect(0.0, 0.0, width, height),
                radiusX: CORNER,
                radiusY: CORNER,
            },
            &r.background,
        );

        // Status row: a dot, a label, and the microphone level.
        let (dot, label) = match status {
            Status::Listening => (&r.recording, "Listening".to_owned()),
            Status::Loading => (&r.waiting, "Recording · loading models…".to_owned()),
            Status::Finishing => (&r.waiting, "Finishing…".to_owned()),
            Status::Message(text) => (&r.waiting, text.clone()),
        };
        let row_center = PADDING + STATUS_HEIGHT / 2.0;
        target.FillEllipse(
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
        target.DrawText(
            &wide,
            &g.status_format,
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
        if matches!(status, Status::Listening | Status::Loading) {
            let x0 = width - PADDING - LEVEL_WIDTH;
            let x1 = x0 + LEVEL_WIDTH;
            let bottom = row_center + LEVEL_HEIGHT / 2.0;
            let step = LEVEL_HEIGHT / SEGMENTS as f32;
            for (i, segment) in meter.segments().enumerate() {
                let y = bottom - i as f32 * step;
                let top = y - step + 1.0;
                let [red, green, blue] = segment.color;
                r.meter.SetColor(&color(red, green, blue));
                r.meter.SetOpacity(if segment.lit { 1.0 } else { 0.18 });
                target.FillRectangle(&rect(x0, top, x1, y), &r.meter);
                if segment.held {
                    target.FillRectangle(&rect(x0, top - 1.0, x1, top), &r.text);
                }
            }
        }

        // The Draft: checked words in white, Unchecked Words in grey. When there are more than
        // 10 lines, the last 10 show.
        let layout = text_layout(g, view, width)?;
        let checked_len = view.checked.encode_utf16().count() as u32;
        let total_len = view.full().encode_utf16().count() as u32;
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
        target.PushAxisAlignedClip(
            &rect(PADDING, top, width - PADDING, height - PADDING + 2.0),
            D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
        );
        target.DrawTextLayout(
            windows_numerics::Vector2 {
                X: PADDING,
                Y: top - offset,
            },
            &layout,
            &r.text,
            D2D1_DRAW_TEXT_OPTIONS_NONE,
        );
        target.PopAxisAlignedClip();
        Ok(())
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
        WM_REPAINT => {
            unsafe {
                let _ = windows::Win32::Graphics::Gdi::InvalidateRect(Some(hwnd), None, false);
            }
            LRESULT(0)
        }
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Draws Scratch Pad frames into memory, for the tests that make a video of a Dictation.
#[cfg(test)]
pub mod offscreen {
    use super::*;
    use windows::Win32::Graphics::DirectWrite::DWRITE_MEASURING_MODE_NATURAL;
    use windows::Win32::Graphics::Imaging::{
        CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICBitmap, IWICImagingFactory,
        WICBitmapCacheOnLoad,
    };
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
    };
    use windows_numerics::Matrix3x2;

    /// Space around the Scratch Pad, in DIPs.
    const MARGIN: f32 = 24.0;
    /// Height of the caption above the Scratch Pad, in DIPs.
    const CAPTION_HEIGHT: f32 = 48.0;

    pub struct Renderer {
        graphics: Graphics,
        caption_format: IDWriteTextFormat,
        bitmap: IWICBitmap,
        target: ID2D1RenderTarget,
        brushes: Brushes,
        width_dip: f32,
        canvas: (f32, f32),
        /// The frame size in pixels.
        pub pixels: (u32, u32),
    }

    impl Renderer {
        /// A canvas for a Scratch Pad `width_dip` wide with up to 10 lines, at `scale` pixels per DIP.
        pub fn new(width_dip: f32, scale: f32) -> Result<Renderer> {
            unsafe {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                let graphics = Graphics::new()?;
                let longest = DraftView {
                    checked: "word ".repeat(400),
                    unchecked: String::new(),
                };
                let canvas = (
                    width_dip + 2.0 * MARGIN,
                    CAPTION_HEIGHT + pad_height(&graphics, &longest, width_dip) + 2.0 * MARGIN,
                );
                let pixels = (even(canvas.0 * scale), even(canvas.1 * scale));
                let wic: IWICImagingFactory =
                    CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
                let bitmap = wic.CreateBitmap(
                    pixels.0,
                    pixels.1,
                    &GUID_WICPixelFormat32bppPBGRA,
                    WICBitmapCacheOnLoad,
                )?;
                let target = graphics.d2d.CreateWicBitmapRenderTarget(
                    &bitmap,
                    &D2D1_RENDER_TARGET_PROPERTIES {
                        pixelFormat: D2D1_PIXEL_FORMAT {
                            format: DXGI_FORMAT_B8G8R8A8_UNORM,
                            alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                        },
                        dpiX: 96.0 * scale,
                        dpiY: 96.0 * scale,
                        ..Default::default()
                    },
                )?;
                let caption_format = graphics.dwrite.CreateTextFormat(
                    w!("Segoe UI"),
                    None,
                    DWRITE_FONT_WEIGHT_NORMAL,
                    DWRITE_FONT_STYLE_NORMAL,
                    DWRITE_FONT_STRETCH_NORMAL,
                    13.0,
                    w!("en-us"),
                )?;
                caption_format.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP)?;
                Ok(Renderer {
                    brushes: Brushes::new(&target)?,
                    graphics,
                    caption_format,
                    bitmap,
                    target,
                    width_dip,
                    canvas,
                    pixels,
                })
            }
        }

        /// Draws one frame: `caption` at the top, and the Scratch Pad at the bottom center, where it
        /// grows upward as on the screen. Returns the pixels as BGRA rows.
        pub fn frame(
            &self,
            caption: &str,
            status: &Status,
            view: &DraftView,
            meter: &PeakMeter,
        ) -> Result<Vec<u8>> {
            let g = &self.graphics;
            let height = pad_height(g, view, self.width_dip);
            unsafe {
                let t = &self.target;
                t.BeginDraw();
                t.SetTransform(&Matrix3x2::identity());
                t.Clear(Some(&color(0.82, 0.83, 0.85)));
                let wide: Vec<u16> = caption.encode_utf16().collect();
                t.DrawText(
                    &wide,
                    &self.caption_format,
                    &rect(MARGIN, 10.0, self.canvas.0 - MARGIN, CAPTION_HEIGHT),
                    &self.brushes.background,
                    D2D1_DRAW_TEXT_OPTIONS_NONE,
                    DWRITE_MEASURING_MODE_NATURAL,
                );
                t.SetTransform(&Matrix3x2::translation(
                    MARGIN,
                    self.canvas.1 - MARGIN - height,
                ));
                draw_pad(
                    t,
                    &self.brushes,
                    g,
                    status,
                    view,
                    meter,
                    self.width_dip,
                    height,
                )?;
                t.EndDraw(None, None)?;
            }
            let stride = self.pixels.0 * 4;
            let mut pixels = vec![0u8; (stride * self.pixels.1) as usize];
            unsafe {
                self.bitmap
                    .CopyPixels(std::ptr::null(), stride, &mut pixels)?
            };
            Ok(pixels)
        }
    }

    /// A video encoder needs even frame sizes.
    fn even(x: f32) -> u32 {
        (x.round() as u32 + 1) & !1
    }
}
