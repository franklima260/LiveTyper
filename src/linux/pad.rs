//! A non-activating, click-through Scratch Pad for KDE Wayland.

use std::cell::RefCell;
use std::rc::Rc;

use gtk4::{glib, pango, prelude::*};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

use crate::draft::DraftView;
use crate::meter::{PeakMeter, SEGMENTS};
use crate::pad_style::*;

pub enum Status<'a> {
    Listening,
    Loading,
    Finishing,
    Message(&'a str),
}

pub struct ScratchPad {
    window: gtk4::ApplicationWindow,
    status: gtk4::Label,
    draft: gtk4::Label,
    meter: gtk4::DrawingArea,
    peak: Rc<RefCell<PeakMeter>>,
}

impl ScratchPad {
    pub fn new(application: &gtk4::Application, width: u32) -> Self {
        let window = gtk4::ApplicationWindow::builder()
            .application(application)
            .default_width(width.clamp(280, 1920) as i32)
            .decorated(false)
            .resizable(false)
            .focusable(false)
            .build();
        window.init_layer_shell();
        window.set_layer(Layer::Overlay);
        window.set_keyboard_mode(KeyboardMode::None);
        window.set_namespace(Some("livetyper-scratch-pad"));
        window.set_anchor(Edge::Bottom, true);
        window.set_margin(Edge::Bottom, BOTTOM_MARGIN as i32);
        window.connect_realize(|window| {
            if let Some(surface) = window.surface() {
                surface.set_input_region(Some(&gtk4::cairo::Region::create()));
            }
        });

        let panel = gtk4::Box::new(gtk4::Orientation::Horizontal, METER_GAP as i32);
        panel.add_css_class("livetyper-panel");
        window.add_css_class("livetyper-window");
        let provider = gtk4::CssProvider::new();
        provider.load_from_data(&format!(
            ".livetyper-window {{ background: transparent; }}
             .livetyper-panel {{ background: #{BACKGROUND:06x}; border: {BORDER}px solid #{OUTLINE:06x};
                                 border-radius: {CORNER_RADIUS}px; padding: {VERTICAL_PADDING}px {HORIZONTAL_PADDING}px; }}
             .livetyper-status {{ color: #{STATUS_TEXT:06x}; font-size: {STATUS_SIZE}px; }}
             .livetyper-draft {{ color: #{CHECKED_TEXT:06x}; font-size: {DRAFT_SIZE}px; }}
             .livetyper-panel scrollbar slider {{ min-width: 4px; min-height: 4px; margin: 0; }}"
        ));
        gtk4::style_context_add_provider_for_display(
            &gtk4::prelude::WidgetExt::display(&window),
            &provider,
            gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );

        let text = gtk4::Box::new(gtk4::Orientation::Vertical, TEXT_GAP as i32);
        text.set_hexpand(true);
        let status = gtk4::Label::new(None);
        status.add_css_class("livetyper-status");
        status.set_xalign(0.0);
        status.set_wrap(true);
        status.set_wrap_mode(pango::WrapMode::WordChar);
        text.append(&status);
        let draft = gtk4::Label::new(None);
        draft.add_css_class("livetyper-draft");
        draft.set_xalign(0.0);
        draft.set_wrap(true);
        draft.set_wrap_mode(pango::WrapMode::WordChar);
        draft.set_max_width_chars(64);
        draft.set_visible(false);
        let scroll = gtk4::ScrolledWindow::builder()
            .hscrollbar_policy(gtk4::PolicyType::Never)
            .vscrollbar_policy(gtk4::PolicyType::External)
            .propagate_natural_height(true)
            .max_content_height(DRAFT_HEIGHT as i32)
            .child(&draft)
            .build();
        let scroll_position = scroll.vadjustment();
        scroll_position.connect_changed(|position| {
            position.set_value(position.upper() - position.page_size());
        });
        text.append(&scroll);
        panel.append(&text);

        let peak = Rc::new(RefCell::new(PeakMeter::default()));
        let draw_peak = peak.clone();
        let meter = gtk4::DrawingArea::builder()
            .content_width(METER_WIDTH as i32)
            .content_height(METER_HEIGHT as i32)
            .valign(gtk4::Align::Start)
            .build();
        meter.set_draw_func(move |_, context, width, height| {
            let height = f64::from(height);
            let width = f64::from(width);
            let gap = f64::from(SEGMENT_GAP);
            let segment_height = (height + gap) / SEGMENTS as f64 - gap;
            for (index, segment) in draw_peak.borrow().segments().enumerate() {
                let y = height - (index as f64 + 1.0) * (segment_height + gap) + gap;
                let [red, green, blue] = segment.color;
                context.set_source_rgba(
                    red.into(),
                    green.into(),
                    blue.into(),
                    if segment.lit {
                        1.0
                    } else {
                        f64::from(UNLIT_OPACITY)
                    },
                );
                context.rectangle(0.0, y, width, segment_height);
                let _ = context.fill();
                if segment.held {
                    context.set_source_rgb(0.95, 0.95, 0.95);
                    context.rectangle(0.0, y, width, 1.0);
                    let _ = context.fill();
                }
            }
        });
        panel.append(&meter);
        window.set_child(Some(&panel));

        Self {
            window,
            status,
            draft,
            meter,
            peak,
        }
    }

    pub fn connect_close(&self, on_close: impl Fn() + 'static) {
        self.window.connect_close_request(move |_| {
            on_close();
            glib::Propagation::Proceed
        });
    }

    pub fn show(&self, status: Status<'_>) {
        *self.peak.borrow_mut() = PeakMeter::default();
        self.meter.queue_draw();
        self.draft.set_visible(false);
        self.status(status);
        self.window.set_visible(true);
    }

    pub fn hide(&self) {
        self.window.set_visible(false);
    }

    pub fn status(&self, status: Status<'_>) {
        let text = match status {
            Status::Listening => "● Listening",
            Status::Loading => "● Recording · loading models…",
            Status::Finishing => "● Finishing…",
            Status::Message(message) => message,
        };
        self.status.set_text(text);
        self.meter
            .set_visible(matches!(status, Status::Listening | Status::Loading));
    }

    pub fn draft(&self, view: &DraftView) {
        let checked = glib::markup_escape_text(&view.checked);
        let unchecked = glib::markup_escape_text(&view.unchecked);
        let space = if checked.is_empty() || unchecked.is_empty() {
            ""
        } else {
            " "
        };
        self.draft.set_markup(&format!(
            "{checked}{space}<span foreground=\"#{UNCHECKED_TEXT:06x}\">{unchecked}</span>"
        ));
        self.draft
            .set_visible(!checked.is_empty() || !unchecked.is_empty());
    }

    pub fn update_meter(&self, level: f32) {
        if self.meter.is_visible() && self.peak.borrow_mut().update(level) {
            self.meter.queue_draw();
        }
    }
}
