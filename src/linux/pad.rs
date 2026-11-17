//! A Scratch Pad with a non-activating overlay on layer-shell desktops.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk4::{gio, glib, pango, prelude::*};
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

use crate::draft::DraftView;
use crate::hotkey::KeyEvent;
use crate::meter::{PeakMeter, SEGMENTS};
use crate::pad_style::*;

const SHELL: &str = "org.gnome.Shell";
const PAD_PATH: &str = "/io/github/franklima260/LiveTyper/ScratchPad";
const PAD_INTERFACE: &str = "io.github.franklima260.LiveTyper.ScratchPad";

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
    shell: Option<ShellPad>,
}

struct ShellPad {
    bus: gio::DBusConnection,
    width: u32,
    visible: Cell<bool>,
    cancel_enabled: Cell<bool>,
    _cancel: gio::SignalSubscription,
    last_update: RefCell<Option<glib::Variant>>,
}

impl ScratchPad {
    pub fn new(
        application: &gtk4::Application,
        width: u32,
        layer_shell: bool,
        on_key: impl Fn(KeyEvent) + 'static,
    ) -> Self {
        let window = gtk4::ApplicationWindow::builder()
            .application(application)
            .default_width(width.clamp(280, 1920) as i32)
            .title("LiveTyper")
            .decorated(!layer_shell)
            .resizable(false)
            .focusable(false)
            .build();
        if layer_shell {
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
        }

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
        draft.set_selectable(!layer_shell);
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
            shell: if layer_shell {
                None
            } else {
                application.dbus_connection().map(|bus| ShellPad {
                    width: width.clamp(280, 1920),
                    visible: Cell::new(false),
                    cancel_enabled: Cell::new(false),
                    _cancel: bus.subscribe_to_signal(
                        Some(SHELL),
                        Some(PAD_INTERFACE),
                        Some("Cancel"),
                        Some(PAD_PATH),
                        None,
                        gio::DBusSignalFlags::NONE,
                        move |_| on_key(KeyEvent::Escape),
                    ),
                    last_update: RefCell::new(None),
                    bus,
                })
            },
        }
    }

    pub fn connect_close(&self, on_close: impl Fn() + 'static) {
        self.window.connect_close_request(move |_| {
            on_close();
            glib::Propagation::Proceed
        });
    }

    pub fn show_overlay(&self, status: Status<'_>) {
        if let Some(shell) = &self.shell {
            self.window.set_visible(false);
            self.reset(status);
            shell.visible.set(true);
        } else {
            self.show(status);
        }
    }

    pub fn show(&self, status: Status<'_>) {
        if let Some(shell) = &self.shell {
            shell.visible.set(false);
        }
        self.reset(status);
        self.window.set_visible(true);
    }

    fn reset(&self, status: Status<'_>) {
        *self.peak.borrow_mut() = PeakMeter::default();
        self.meter.queue_draw();
        self.draft.set_visible(false);
        self.status(status);
    }

    pub fn hide(&self) {
        if let Some(shell) = &self.shell {
            shell.visible.set(false);
        }
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
        let visible = self.meter.is_visible()
            || self
                .shell
                .as_ref()
                .is_some_and(|shell| shell.visible.get() && self.meter.get_visible());
        if visible && self.peak.borrow_mut().update(level) {
            self.meter.queue_draw();
        }
    }

    pub fn shell_bus(&self) -> Option<gio::DBusConnection> {
        self.shell.as_ref().map(|shell| shell.bus.clone())
    }

    pub fn set_escape(&self, enabled: bool) {
        if let Some(shell) = &self.shell {
            shell.cancel_enabled.set(enabled);
        }
    }

    pub fn flush_overlay(&self) {
        let Some(shell) = &self.shell else { return };
        let visible = shell.visible.get();
        let frame = if visible {
            let draft = if self.draft.get_visible() {
                self.draft.label()
            } else {
                glib::GString::default()
            };
            let segments: Vec<_> = self
                .peak
                .borrow()
                .segments()
                .map(|segment| {
                    let [red, green, blue] = segment.color.map(f64::from);
                    (segment.lit, segment.held, red, green, blue)
                })
                .collect();
            (
                true,
                self.status.text().as_str(),
                draft.as_str(),
                self.meter.get_visible(),
                shell.width,
                segments,
                shell.cancel_enabled.get(),
            )
                .to_variant()
        } else {
            (
                false,
                "",
                "",
                false,
                shell.width,
                Vec::<(bool, bool, f64, f64, f64)>::new(),
                shell.cancel_enabled.get(),
            )
                .to_variant()
        };
        if shell.last_update.borrow().as_ref() != Some(&frame) {
            if let Err(error) = shell.bus.emit_signal(
                Some(SHELL),
                PAD_PATH,
                PAD_INTERFACE,
                "Changed",
                Some(&frame),
            ) {
                log::warn!("GNOME Scratch Pad: {error}");
            }
            shell.last_update.replace(Some(frame));
        }
    }
}

pub async fn paste_target(bus: &gio::DBusConnection) -> Option<(u32, bool)> {
    bus.call_future(
        Some(SHELL),
        PAD_PATH,
        PAD_INTERFACE,
        "GetPasteTarget",
        None,
        None,
        gio::DBusCallFlags::NO_AUTO_START,
        1000,
    )
    .await
    .ok()?
    .get()
}

pub async fn talk_control_held(bus: &gio::DBusConnection) -> Option<bool> {
    let (held,) = bus
        .call_future(
            Some(SHELL),
            PAD_PATH,
            PAD_INTERFACE,
            "GetTalkControl",
            None,
            None,
            gio::DBusCallFlags::NO_AUTO_START,
            1000,
        )
        .await
        .ok()?
        .get::<(bool,)>()?;
    Some(held)
}
