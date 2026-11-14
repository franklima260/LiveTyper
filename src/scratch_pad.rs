//! The Scratch Pad: a read-only window that shows the Draft during a Dictation.
//!
//! It never takes the focus and lets mouse clicks pass through (`WS_EX_NOACTIVATE`,
//! `WS_EX_TRANSPARENT`), so the caret stays in the Target Text Box. It is drawn with Direct2D and
//! DirectWrite. The pad grows upward as the draft wraps.
//!
//! The Direct2D and DirectWrite objects use much memory (Direct2D loads the graphics driver), so
//! they are made on the first show, and the render target is released when the Scratch Pad hides.

use windows::Win32::Foundation::{
    COLORREF, E_FAIL, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM,
};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_DRAW_TEXT_OPTIONS_NONE,
    D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_RENDER_TARGET_PROPERTIES, D2D1_ROUNDED_RECT,
    D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE, D2D1CreateFactory, ID2D1DCRenderTarget, ID2D1Factory,
    ID2D1RenderTarget, ID2D1SolidColorBrush,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_ISOLATED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_LINE_SPACING_METHOD_UNIFORM, DWRITE_TEXT_METRICS,
    DWRITE_TEXT_RANGE, DWRITE_WORD_WRAPPING_WRAP, DWriteCreateFactory, IDWriteFactory,
    IDWriteFactory5, IDWriteFontCollection, IDWriteInMemoryFontFileLoader, IDWriteTextFormat,
    IDWriteTextLayout,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::Graphics::Gdi::{
    AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION, BeginPaint,
    CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, EndPaint,
    HBITMAP, HDC, PAINTSTRUCT, SelectObject, ValidateRect,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, HTTRANSPARENT, HWND_TOPMOST, MA_NOACTIVATE, PostMessageW,
    RegisterClassW, SPI_GETWORKAREA, SW_HIDE, SWP_NOACTIVATE, SWP_SHOWWINDOW,
    SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SetWindowPos, ShowWindow, SystemParametersInfoW,
    ULW_ALPHA, UpdateLayeredWindow, WM_APP, WM_MOUSEACTIVATE, WM_NCHITTEST, WM_PAINT, WNDCLASSW,
    WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};
use windows::core::{Error, Interface, Result, w};

use crate::draft::DraftView;
use crate::meter::{PeakMeter, SEGMENTS};

use crate::pad_style::*;

const INSET_X: f32 = (HORIZONTAL_PADDING + BORDER) as f32;
const INSET_Y: f32 = (VERTICAL_PADDING + BORDER) as f32;

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
    target: ID2D1DCRenderTarget,
    brushes: Brushes,
    surface: LayerBitmap,
}

struct LayerBitmap {
    dc: HDC,
    bitmap: HBITMAP,
    #[cfg(test)]
    bits: *const u8,
}

impl LayerBitmap {
    fn new(width: i32, height: i32) -> Result<Self> {
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        unsafe {
            let mut bits = std::ptr::null_mut();
            let bitmap = CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0)?;
            let dc = CreateCompatibleDC(None);
            if dc.is_invalid() || SelectObject(dc, bitmap.into()).is_invalid() {
                let error = Error::from_thread();
                if !dc.is_invalid() {
                    let _ = DeleteDC(dc);
                }
                let _ = DeleteObject(bitmap.into());
                return Err(error);
            }
            Ok(Self {
                dc,
                bitmap,
                #[cfg(test)]
                bits: bits.cast(),
            })
        }
    }
}

impl Drop for LayerBitmap {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteDC(self.dc);
            let _ = DeleteObject(self.bitmap.into());
        }
    }
}

struct PadFont {
    factory: IDWriteFactory5,
    loader: IDWriteInMemoryFontFileLoader,
}

impl PadFont {
    fn new(factory: &IDWriteFactory) -> Result<Self> {
        unsafe {
            let factory: IDWriteFactory5 = factory.cast()?;
            let loader = factory.CreateInMemoryFontFileLoader()?;
            factory.RegisterFontFileLoader(&loader)?;
            Ok(Self { factory, loader })
        }
    }

    fn collection(&self) -> Result<IDWriteFontCollection> {
        let data = include_bytes!("../assets/fonts/DejaVuSans.ttf");
        unsafe {
            let file = self.loader.CreateInMemoryFontFileReference(
                &self.factory,
                data.as_ptr().cast(),
                data.len() as u32,
                None,
            )?;
            let builder = self.factory.CreateFontSetBuilder()?;
            builder.AddFontFile(&file)?;
            self.factory
                .CreateFontCollectionFromFontSet(&builder.CreateFontSet()?)?
                .cast()
        }
    }
}

impl Drop for PadFont {
    fn drop(&mut self) {
        unsafe {
            let _ = self.factory.UnregisterFontFileLoader(&self.loader);
        }
    }
}

/// The brushes of one render target.
struct Brushes {
    background: ID2D1SolidColorBrush,
    text: ID2D1SolidColorBrush,
    unchecked: ID2D1SolidColorBrush,
    muted: ID2D1SolidColorBrush,
    outline: ID2D1SolidColorBrush,
    meter: ID2D1SolidColorBrush,
}

impl Brushes {
    fn new(target: &ID2D1RenderTarget) -> Result<Brushes> {
        let brush = |rgb| unsafe { target.CreateSolidColorBrush(&rgb_color(rgb), None) };
        Ok(Brushes {
            background: brush(BACKGROUND)?,
            text: brush(CHECKED_TEXT)?,
            unchecked: brush(UNCHECKED_TEXT)?,
            muted: brush(STATUS_TEXT)?,
            outline: brush(OUTLINE)?,
            meter: brush(0)?,
        })
    }
}

/// The factories and text formats. Made on the first show, then kept.
struct Graphics {
    d2d: ID2D1Factory,
    dwrite: IDWriteFactory,
    text_format: IDWriteTextFormat,
    status_format: IDWriteTextFormat,
    _font: PadFont,
}

impl Graphics {
    fn new() -> Result<Graphics> {
        unsafe {
            let d2d: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_ISOLATED)?;
            let font = PadFont::new(&dwrite)?;
            let collection = font.collection()?;
            let text_format = dwrite.CreateTextFormat(
                w!("DejaVu Sans"),
                &collection,
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                DRAFT_SIZE as f32,
                w!("en-us"),
            )?;
            text_format.SetLineSpacing(DWRITE_LINE_SPACING_METHOD_UNIFORM, 21.0, 16.0)?;
            text_format.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP)?;
            let status_format = dwrite.CreateTextFormat(
                w!("DejaVu Sans"),
                &collection,
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                STATUS_SIZE as f32,
                w!("en-us"),
            )?;
            status_format.SetLineSpacing(DWRITE_LINE_SPACING_METHOD_UNIFORM, 15.0, 12.0)?;
            status_format.SetWordWrapping(DWRITE_WORD_WRAPPING_WRAP)?;
            Ok(Graphics {
                d2d,
                dwrite,
                text_format,
                status_format,
                _font: font,
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
            Ok(g) => pad_height(g, &self.status, &self.view, self.width_dip),
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
            self.resources = None;
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
            let (width, height) = self.size;
            let surface = LayerBitmap::new(width, height)?;
            let target = g.d2d.CreateDCRenderTarget(&D2D1_RENDER_TARGET_PROPERTIES {
                pixelFormat: D2D1_PIXEL_FORMAT {
                    format: DXGI_FORMAT_B8G8R8A8_UNORM,
                    alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
                },
                dpiX: dpi,
                dpiY: dpi,
                ..Default::default()
            })?;
            target.BindDC(
                surface.dc,
                &RECT {
                    left: 0,
                    top: 0,
                    right: width,
                    bottom: height,
                },
            )?;
            target.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_GRAYSCALE);
            Ok(Resources {
                brushes: Brushes::new(&target)?,
                target,
                surface,
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
            r.target.Clear(Some(&D2D1_COLOR_F::default()));
            let result = draw_pad(
                &r.target,
                &r.brushes,
                g,
                &self.status,
                &self.view,
                &self.meter,
                width,
                height,
            );
            let end = r.target.EndDraw(None, None);
            result.and(end)?;
            // Preserve the fractional alpha at the edge when Windows composites the panel.
            UpdateLayeredWindow(
                self.hwnd,
                None,
                None,
                Some(&SIZE {
                    cx: self.size.0,
                    cy: self.size.1,
                }),
                Some(r.surface.dc),
                Some(&POINT::default()),
                COLORREF(0),
                Some(&BLENDFUNCTION {
                    BlendOp: AC_SRC_OVER as u8,
                    BlendFlags: 0,
                    SourceConstantAlpha: 255,
                    AlphaFormat: AC_SRC_ALPHA as u8,
                }),
                ULW_ALPHA,
            )
        }
    }
}

impl Status {
    fn label(&self) -> &str {
        match self {
            Self::Listening => "● Listening",
            Self::Loading => "● Recording · loading models…",
            Self::Finishing => "● Finishing…",
            Self::Message(text) => text,
        }
    }

    fn shows_meter(&self) -> bool {
        matches!(self, Self::Listening | Self::Loading)
    }
}

fn text_width(status: &Status, width: f32) -> f32 {
    let meter_space = if status.shows_meter() {
        (METER_WIDTH + METER_GAP) as f32
    } else {
        0.0
    };
    width - 2.0 * INSET_X - meter_space
}

fn text_layout(
    g: &Graphics,
    text: &str,
    format: &IDWriteTextFormat,
    width: f32,
) -> Result<IDWriteTextLayout> {
    let wide: Vec<u16> = text.encode_utf16().collect();
    unsafe { g.dwrite.CreateTextLayout(&wide, format, width, f32::MAX) }
}

fn text_height(layout: &IDWriteTextLayout) -> Result<f32> {
    let mut metrics = DWRITE_TEXT_METRICS::default();
    unsafe { layout.GetMetrics(&mut metrics)? };
    Ok(metrics.height)
}

fn pad_height(g: &Graphics, status: &Status, view: &DraftView, width: f32) -> f32 {
    let width = text_width(status, width);
    let status_height = text_layout(g, status.label(), &g.status_format, width)
        .and_then(|layout| text_height(&layout))
        .unwrap_or(STATUS_SIZE as f32);
    let draft = view.full();
    let text_height = if draft.is_empty() {
        status_height
    } else {
        let height = text_layout(g, &draft, &g.text_format, width)
            .and_then(|layout| text_height(&layout))
            .unwrap_or(DRAFT_SIZE as f32);
        status_height + TEXT_GAP as f32 + height.min(DRAFT_HEIGHT as f32)
    };
    let meter_height = if status.shows_meter() {
        METER_HEIGHT as f32
    } else {
        0.0
    };
    2.0 * INSET_Y + text_height.max(meter_height)
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
        let radius = CORNER_RADIUS as f32;
        let border = BORDER as f32;
        target.FillRoundedRectangle(
            &D2D1_ROUNDED_RECT {
                rect: rect(0.0, 0.0, width, height),
                radiusX: radius,
                radiusY: radius,
            },
            &r.outline,
        );
        target.FillRoundedRectangle(
            &D2D1_ROUNDED_RECT {
                rect: rect(border, border, width - border, height - border),
                radiusX: radius - border,
                radiusY: radius - border,
            },
            &r.background,
        );

        let width = text_width(status, width);
        let status_layout = text_layout(g, status.label(), &g.status_format, width)?;
        target.DrawTextLayout(
            windows_numerics::Vector2 {
                X: INSET_X,
                Y: INSET_Y,
            },
            &status_layout,
            &r.muted,
            D2D1_DRAW_TEXT_OPTIONS_NONE,
        );
        if status.shows_meter() {
            let x0 = INSET_X + width + METER_GAP as f32;
            let x1 = x0 + METER_WIDTH as f32;
            let bottom = INSET_Y + METER_HEIGHT as f32;
            let step = (METER_HEIGHT as f32 + SEGMENT_GAP) / SEGMENTS as f32;
            for (i, segment) in meter.segments().enumerate() {
                let y = bottom - i as f32 * step;
                let top = y - step + SEGMENT_GAP;
                let [red, green, blue] = segment.color;
                r.meter.SetColor(&color(red, green, blue));
                r.meter
                    .SetOpacity(if segment.lit { 1.0 } else { UNLIT_OPACITY });
                target.FillRectangle(&rect(x0, top, x1, y), &r.meter);
                if segment.held {
                    target.FillRectangle(&rect(x0, top, x1, top + 1.0), &r.text);
                }
            }
        }

        let draft = view.full();
        if draft.is_empty() {
            return Ok(());
        }
        let layout = text_layout(g, &draft, &g.text_format, width)?;
        let checked_len = view.checked.encode_utf16().count() as u32;
        let total_len = draft.encode_utf16().count() as u32;
        if total_len > checked_len {
            layout.SetDrawingEffect(
                &r.unchecked.cast::<windows::core::IUnknown>()?,
                DWRITE_TEXT_RANGE {
                    startPosition: checked_len,
                    length: total_len - checked_len,
                },
            )?;
        }
        let top = INSET_Y + text_height(&status_layout)? + TEXT_GAP as f32;
        let visible_height = (height - top - INSET_Y).clamp(0.0, DRAFT_HEIGHT as f32);
        let offset = (text_height(&layout)? - visible_height).max(0.0);
        target.PushAxisAlignedClip(
            &rect(INSET_X, top, INSET_X + width, top + visible_height),
            D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
        );
        target.DrawTextLayout(
            windows_numerics::Vector2 {
                X: INSET_X,
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

fn rgb_color(rgb: u32) -> D2D1_COLOR_F {
    let channel = |shift: u32| ((rgb >> shift) & 0xff) as f32 / 255.0;
    color(channel(16), channel(8), channel(0))
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
        area.bottom - (BOTTOM_MARGIN as f32 * scale) as i32,
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
        /// A canvas for a Scratch Pad `width_dip` wide with a capped draft area, at `scale` pixels per DIP.
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
                    CAPTION_HEIGHT
                        + pad_height(&graphics, &Status::Listening, &longest, width_dip)
                        + 2.0 * MARGIN,
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
            let height = pad_height(g, status, view, self.width_dip);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_surface_has_transparent_corners_and_an_antialiased_border() {
        let mut pad = ScratchPad::new(None, 320).expect("create Scratch Pad");
        pad.show(Status::Listening);
        for view in [
            DraftView::default(),
            DraftView {
                checked: "The checked draft should wrap as the Scratch Pad grows. ".repeat(6),
                unchecked: "These words are still unchecked.".into(),
            },
        ] {
            pad.set_draft(view);
            pad.paint();
            let (width, height) = pad.size;
            let surface = &pad
                .resources
                .as_ref()
                .expect("draw the native surface")
                .surface;
            let pixels =
                unsafe { std::slice::from_raw_parts(surface.bits, (width * height * 4) as usize) };
            let pixel = |x, y| {
                let start = ((y * width + x) * 4) as usize;
                &pixels[start..start + 4]
            };
            assert_eq!(pixel(0, 0), [0, 0, 0, 0]);
            assert_eq!(pixel(width - 1, height - 1), [0, 0, 0, 0]);
            assert_eq!(pixel(width / 2, 0), [0x3d, 0x34, 0x34, 255]);
            assert_eq!(pixel(width / 2, height - 1), [0x3d, 0x34, 0x34, 255]);
            assert!(
                (0..12).any(|y| (0..12).any(|x| (1..255).contains(&pixel(x, y)[3]))),
                "the rounded edge must contain partially transparent pixels"
            );
        }
        pad.hide();
        unsafe {
            windows::Win32::UI::WindowsAndMessaging::DestroyWindow(pad.hwnd).unwrap();
        }
    }
}
