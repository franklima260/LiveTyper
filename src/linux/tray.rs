//! The Linux tray icon and its menu, as a StatusNotifierItem with a D-Bus menu.
//!
//! KDE Plasma shows it directly. GNOME shows it only with the AppIndicator extension (Ubuntu has it
//! on by default). Without a tray host the icon simply does not show, and when a host starts
//! later (for example, after a Plasma restart) the icon registers again.
//!
//! The icon is `assets/tray-icon.png`: white when LiveTyper is ready, red while recording and amber
//! while finishing. `assets/tray-icon-muted.png` shows when dictation is paused, the microphone is
//! closed, or LiveTyper cannot dictate.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use gtk4::{gio, glib, prelude::*};

use gtk4::gdk_pixbuf::{InterpType, Pixbuf};

use crate::icon_shape;

const ICON_PNG: &[u8] = include_bytes!("../../assets/tray-icon.png");
const MUTED_ICON_PNG: &[u8] = include_bytes!("../../assets/tray-icon-muted.png");
/// The icon goes to the tray host in these sizes; it picks the one that fits its panel.
const SIZES: [i32; 4] = [16, 22, 32, 64];

const ITEM_PATH: &str = "/StatusNotifierItem";
const MENU_PATH: &str = "/MenuBar";
const ITEM_INTERFACE: &str = "org.kde.StatusNotifierItem";
const WATCHER: &str = "org.kde.StatusNotifierWatcher";

const ITEM_XML: &str = r#"<node>
  <interface name="org.kde.StatusNotifierItem">
    <property name="Category" type="s" access="read"/>
    <property name="Id" type="s" access="read"/>
    <property name="Title" type="s" access="read"/>
    <property name="Status" type="s" access="read"/>
    <property name="WindowId" type="i" access="read"/>
    <property name="IconName" type="s" access="read"/>
    <property name="IconPixmap" type="a(iiay)" access="read"/>
    <property name="OverlayIconName" type="s" access="read"/>
    <property name="OverlayIconPixmap" type="a(iiay)" access="read"/>
    <property name="AttentionIconName" type="s" access="read"/>
    <property name="AttentionIconPixmap" type="a(iiay)" access="read"/>
    <property name="AttentionMovieName" type="s" access="read"/>
    <property name="IconThemePath" type="s" access="read"/>
    <property name="ToolTip" type="(sa(iiay)ss)" access="read"/>
    <property name="ItemIsMenu" type="b" access="read"/>
    <property name="Menu" type="o" access="read"/>
    <method name="ContextMenu"><arg name="x" type="i" direction="in"/><arg name="y" type="i" direction="in"/></method>
    <method name="Activate"><arg name="x" type="i" direction="in"/><arg name="y" type="i" direction="in"/></method>
    <method name="SecondaryActivate"><arg name="x" type="i" direction="in"/><arg name="y" type="i" direction="in"/></method>
    <method name="Scroll"><arg name="delta" type="i" direction="in"/><arg name="orientation" type="s" direction="in"/></method>
    <signal name="NewIcon"/>
    <signal name="NewToolTip"/>
    <signal name="NewStatus"><arg name="status" type="s"/></signal>
  </interface>
</node>"#;

const MENU_XML: &str = r#"<node>
  <interface name="com.canonical.dbusmenu">
    <property name="Version" type="u" access="read"/>
    <property name="TextDirection" type="s" access="read"/>
    <property name="Status" type="s" access="read"/>
    <property name="IconThemePath" type="as" access="read"/>
    <method name="GetLayout">
      <arg name="parentId" type="i" direction="in"/>
      <arg name="recursionDepth" type="i" direction="in"/>
      <arg name="propertyNames" type="as" direction="in"/>
      <arg name="revision" type="u" direction="out"/>
      <arg name="layout" type="(ia{sv}av)" direction="out"/>
    </method>
    <method name="GetGroupProperties">
      <arg name="ids" type="ai" direction="in"/>
      <arg name="propertyNames" type="as" direction="in"/>
      <arg name="properties" type="a(ia{sv})" direction="out"/>
    </method>
    <method name="GetProperty">
      <arg name="id" type="i" direction="in"/>
      <arg name="name" type="s" direction="in"/>
      <arg name="value" type="v" direction="out"/>
    </method>
    <method name="Event">
      <arg name="id" type="i" direction="in"/>
      <arg name="eventId" type="s" direction="in"/>
      <arg name="data" type="v" direction="in"/>
      <arg name="timestamp" type="u" direction="in"/>
    </method>
    <method name="EventGroup">
      <arg name="events" type="a(isvu)" direction="in"/>
      <arg name="idErrors" type="ai" direction="out"/>
    </method>
    <method name="AboutToShow">
      <arg name="id" type="i" direction="in"/>
      <arg name="needUpdate" type="b" direction="out"/>
    </method>
    <method name="AboutToShowGroup">
      <arg name="ids" type="ai" direction="in"/>
      <arg name="updatesNeeded" type="ai" direction="out"/>
      <arg name="idErrors" type="ai" direction="out"/>
    </method>
    <signal name="LayoutUpdated"><arg name="revision" type="u"/><arg name="parent" type="i"/></signal>
    <signal name="ItemsPropertiesUpdated">
      <arg name="updatedProps" type="a(ia{sv})"/>
      <arg name="removedProps" type="a(ias)"/>
    </signal>
  </interface>
</node>"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuCommand {
    TextTrace,
    TogglePause,
    OpenSettings,
    OpenLog,
    Quit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IconState {
    /// Ready: the Talk Key starts a Dictation.
    Ready,
    Recording,
    /// The last Recast runs, or the text is pasted.
    Busy,
    /// Paused, microphone closed, or LiveTyper cannot dictate.
    Muted,
}

type Pixmaps = Vec<(i32, i32, Vec<u8>)>;

struct Shown {
    icon: IconState,
    tip: String,
}

pub struct Tray {
    bus: gio::DBusConnection,
    shown: Rc<RefCell<Shown>>,
    paused: Rc<Cell<bool>>,
    revision: Rc<Cell<u32>>,
    _item: gio::RegistrationId,
    _menu: gio::RegistrationId,
}

impl Tray {
    pub fn new(
        bus: &gio::DBusConnection,
        on_command: impl Fn(MenuCommand) + 'static,
    ) -> Result<Tray, String> {
        let shown = Rc::new(RefCell::new(Shown {
            icon: IconState::Ready,
            tip: String::new(),
        }));
        let paused = Rc::new(Cell::new(false));
        let revision = Rc::new(Cell::new(1u32));
        let icons: Rc<HashMap<IconState, Pixmaps>> = Rc::new(
            [
                IconState::Ready,
                IconState::Recording,
                IconState::Busy,
                IconState::Muted,
            ]
            .into_iter()
            .map(|state| (state, pixmaps(state)))
            .collect(),
        );

        let item_info = interface(ITEM_XML, ITEM_INTERFACE)?;
        let property_shown = shown.clone();
        let item = bus
            .register_object(ITEM_PATH, &item_info)
            .method_call(|_, _, _, _, _, _, invocation| invocation.return_value(None))
            .property(move |_, _, _, _, name| {
                let shown = property_shown.borrow();
                item_property(name, &icons[&shown.icon], &shown.tip)
            })
            .build()
            .map_err(|e| format!("tray icon: {e}"))?;

        let menu_info = interface(MENU_XML, "com.canonical.dbusmenu")?;
        let menu_paused = paused.clone();
        let menu_revision = revision.clone();
        let menu = bus
            .register_object(MENU_PATH, &menu_info)
            .method_call(move |_, _, _, _, method, params, invocation| {
                let reply = menu_call(
                    method,
                    &params,
                    &menu_items(menu_paused.get()),
                    menu_revision.get(),
                    &on_command,
                );
                invocation.return_value(reply.as_ref());
            })
            .property(|_, _, _, _, name| match name {
                "Version" => 3u32.to_variant(),
                "TextDirection" => "ltr".to_variant(),
                "Status" => "normal".to_variant(),
                _ => Vec::<String>::new().to_variant(),
            })
            .build()
            .map_err(|e| format!("tray menu: {e}"))?;

        // Register with the tray host now, and again whenever a host starts. The watch lasts as
        // long as LiveTyper runs.
        let _ = gio::bus_watch_name_on_connection(
            bus,
            WATCHER,
            gio::BusNameWatcherFlags::NONE,
            |bus, _, _| {
                let Some(name) = bus.unique_name() else {
                    return;
                };
                glib::spawn_future_local(async move {
                    if let Err(error) = bus
                        .call_future(
                            Some(WATCHER),
                            "/StatusNotifierWatcher",
                            WATCHER,
                            "RegisterStatusNotifierItem",
                            Some(&(name.as_str(),).to_variant()),
                            None,
                            gio::DBusCallFlags::NONE,
                            2000,
                        )
                        .await
                    {
                        log::warn!("tray icon: {error}");
                    }
                });
            },
            |_, _| {},
        );
        Ok(Tray {
            bus: bus.clone(),
            shown,
            paused,
            revision,
            _item: item,
            _menu: menu,
        })
    }

    /// Shows `icon`, with `tip` under "LiveTyper" in the tooltip.
    pub fn set_state(&self, icon: IconState, tip: &str) {
        let mut shown = self.shown.borrow_mut();
        let new_icon = shown.icon != icon;
        let new_tip = shown.tip != tip;
        shown.icon = icon;
        if new_tip {
            shown.tip = tip.to_owned();
        }
        drop(shown);
        for (changed, signal) in [(new_icon, "NewIcon"), (new_tip, "NewToolTip")] {
            if changed {
                let _ = self
                    .bus
                    .emit_signal(None, ITEM_PATH, ITEM_INTERFACE, signal, None);
            }
        }
    }

    /// Switches the menu item between "Pause dictation" and "Resume dictation".
    pub fn set_paused(&self, paused: bool) {
        if self.paused.replace(paused) == paused {
            return;
        }
        self.revision.set(self.revision.get() + 1);
        let _ = self.bus.emit_signal(
            None,
            MENU_PATH,
            "com.canonical.dbusmenu",
            "LayoutUpdated",
            Some(&(self.revision.get(), 0i32).to_variant()),
        );
    }
}

fn interface(xml: &str, name: &str) -> Result<gio::DBusInterfaceInfo, String> {
    gio::DBusNodeInfo::for_xml(xml)
        .map_err(|e| e.to_string())?
        .lookup_interface(name)
        .ok_or_else(|| format!("no {name} interface"))
}

fn item_property(name: &str, icon: &Pixmaps, tip: &str) -> glib::Variant {
    let no_pixmaps = Pixmaps::new();
    match name {
        "Category" => "ApplicationStatus".to_variant(),
        "Id" => "livetyper".to_variant(),
        "Title" => "LiveTyper".to_variant(),
        "Status" => "Active".to_variant(),
        "WindowId" => 0i32.to_variant(),
        "IconPixmap" => icon.to_variant(),
        "OverlayIconPixmap" | "AttentionIconPixmap" => no_pixmaps.to_variant(),
        "ToolTip" => (
            String::new(),
            no_pixmaps,
            "LiveTyper".to_owned(),
            tip.to_owned(),
        )
            .to_variant(),
        "ItemIsMenu" => true.to_variant(),
        "Menu" => glib::variant::ObjectPath::try_from(MENU_PATH.to_owned())
            .expect("valid object path")
            .to_variant(),
        // IconName (empty: the pixmap is the icon), OverlayIconName, AttentionIconName,
        // AttentionMovieName, IconThemePath.
        _ => "".to_variant(),
    }
}

/// The icon of a state in several sizes, as ARGB32 in network byte order. The PNGs are white;
/// recording and finishing tint them.
fn pixmaps(state: IconState) -> Pixmaps {
    let (png, tint) = match state {
        IconState::Ready => (ICON_PNG, [255, 255, 255]),
        IconState::Recording => (ICON_PNG, [229, 72, 77]),
        IconState::Busy => (ICON_PNG, [245, 165, 36]),
        IconState::Muted => (MUTED_ICON_PNG, [255, 255, 255]),
    };
    match Pixbuf::from_read(std::io::Cursor::new(png)) {
        Ok(pixbuf) => SIZES
            .into_iter()
            .filter_map(|size| {
                let scaled = pixbuf.scale_simple(size, size, InterpType::Hyper)?;
                Some((size, size, argb(&scaled, tint)))
            })
            .collect(),
        Err(error) => {
            log::warn!("tray icon image: {error}; using the built-in microphone");
            fallback(tint)
        }
    }
}

/// Converts RGB(A) pixels to tinted ARGB32 in network byte order.
fn argb(pixbuf: &Pixbuf, tint: [u8; 3]) -> Vec<u8> {
    let (width, height) = (pixbuf.width() as usize, pixbuf.height() as usize);
    let channels = pixbuf.n_channels() as usize;
    let stride = pixbuf.rowstride() as usize;
    let pixels = pixbuf.read_pixel_bytes();
    let scale = |value: u8, tint: u8| (u16::from(value) * u16::from(tint) / 255) as u8;
    let mut out = Vec::with_capacity(width * height * 4);
    for y in 0..height {
        for x in 0..width {
            let p = &pixels[y * stride + x * channels..][..channels];
            let alpha = if pixbuf.has_alpha() { p[3] } else { 255 };
            out.extend([
                alpha,
                scale(p[0], tint[0]),
                scale(p[1], tint[1]),
                scale(p[2], tint[2]),
            ]);
        }
    }
    out
}

/// The drawn microphone, if the PNG cannot be read.
fn fallback(rgb: [u8; 3]) -> Pixmaps {
    SIZES
        .into_iter()
        .map(|size| {
            let mut bytes = Vec::with_capacity((size * size * 4) as usize);
            for alpha in icon_shape::alpha(size as usize) {
                bytes.extend([(alpha * 255.0) as u8, rgb[0], rgb[1], rgb[2]]);
            }
            (size, size, bytes)
        })
        .collect()
}

/// One menu item: id, label (`None` for a separator), and command.
type Item = (i32, Option<&'static str>, Option<MenuCommand>);

fn menu_items(paused: bool) -> Vec<Item> {
    vec![
        (1, Some("Text Trace…"), Some(MenuCommand::TextTrace)),
        (
            2,
            Some(if paused {
                "Resume dictation"
            } else {
                "Pause dictation"
            }),
            Some(MenuCommand::TogglePause),
        ),
        (3, None, None),
        (4, Some("Open settings"), Some(MenuCommand::OpenSettings)),
        (5, Some("Open log"), Some(MenuCommand::OpenLog)),
        (6, None, None),
        (7, Some("Quit"), Some(MenuCommand::Quit)),
    ]
}

fn item_props(label: Option<&str>) -> HashMap<String, glib::Variant> {
    let mut props = HashMap::new();
    match label {
        Some(label) => {
            props.insert("label".to_owned(), label.to_variant());
            props.insert("enabled".to_owned(), true.to_variant());
        }
        None => {
            props.insert("type".to_owned(), "separator".to_variant());
        }
    }
    props.insert("visible".to_owned(), true.to_variant());
    props
}

fn layout(items: &[Item]) -> glib::Variant {
    let children = items.iter().map(|(id, label, _)| {
        let child = glib::Variant::tuple_from_iter([
            id.to_variant(),
            item_props(*label).to_variant(),
            glib::Variant::array_from_iter_with_type(
                glib::VariantTy::VARIANT,
                Vec::<glib::Variant>::new(),
            ),
        ]);
        glib::Variant::from_variant(&child)
    });
    let mut root = HashMap::new();
    root.insert("children-display".to_owned(), "submenu".to_variant());
    glib::Variant::tuple_from_iter([
        0i32.to_variant(),
        root.to_variant(),
        glib::Variant::array_from_iter_with_type(glib::VariantTy::VARIANT, children),
    ])
}

fn menu_call(
    method: &str,
    params: &glib::Variant,
    items: &[Item],
    revision: u32,
    on_command: &impl Fn(MenuCommand),
) -> Option<glib::Variant> {
    match method {
        "GetLayout" => Some(glib::Variant::tuple_from_iter([
            revision.to_variant(),
            layout(items),
        ])),
        "GetGroupProperties" => {
            let ids = params.child_value(0).get::<Vec<i32>>().unwrap_or_default();
            let props: Vec<(i32, HashMap<String, glib::Variant>)> = items
                .iter()
                .filter(|(id, _, _)| ids.is_empty() || ids.contains(id))
                .map(|(id, label, _)| (*id, item_props(*label)))
                .collect();
            Some((props,).to_variant())
        }
        "GetProperty" => {
            let id = params.child_value(0).get::<i32>().unwrap_or(-1);
            let name = params.child_value(1).get::<String>().unwrap_or_default();
            let value = items
                .iter()
                .find(|(item, _, _)| *item == id)
                .and_then(|(_, label, _)| item_props(*label).remove(&name))
                .unwrap_or_else(|| "".to_variant());
            Some(glib::Variant::tuple_from_iter([
                glib::Variant::from_variant(&value),
            ]))
        }
        "Event" => {
            let id = params.child_value(0).get::<i32>().unwrap_or(-1);
            let event = params.child_value(1).get::<String>().unwrap_or_default();
            if event == "clicked" {
                run(items, id, on_command);
            }
            None
        }
        "EventGroup" => {
            let events = params.child_value(0);
            for i in 0..events.n_children() {
                let event = events.child_value(i);
                let id = event.child_value(0).get::<i32>().unwrap_or(-1);
                if event.child_value(1).get::<String>().as_deref() == Some("clicked") {
                    run(items, id, on_command);
                }
            }
            Some((Vec::<i32>::new(),).to_variant())
        }
        "AboutToShow" => Some((false,).to_variant()),
        "AboutToShowGroup" => Some((Vec::<i32>::new(), Vec::<i32>::new()).to_variant()),
        _ => None,
    }
}

fn run(items: &[Item], id: i32, on_command: &impl Fn(MenuCommand)) {
    if let Some(command) = items
        .iter()
        .find(|(item, _, _)| *item == id)
        .and_then(|(_, _, command)| *command)
    {
        on_command(command);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn click(id: i32) -> glib::Variant {
        (
            id,
            "clicked",
            glib::Variant::from_variant(&0i32.to_variant()),
            0u32,
        )
            .to_variant()
    }

    #[test]
    fn the_interfaces_parse() {
        assert!(interface(ITEM_XML, ITEM_INTERFACE).is_ok());
        assert!(interface(MENU_XML, "com.canonical.dbusmenu").is_ok());
    }

    #[test]
    fn layout_has_the_dbusmenu_type_and_every_item() {
        let items = menu_items(false);
        let layout = layout(&items);
        assert_eq!(layout.type_().as_str(), "(ia{sv}av)");
        assert_eq!(layout.child_value(2).n_children(), items.len());
        let reply = menu_call("GetLayout", &().to_variant(), &items, 3, &|_| {}).unwrap();
        assert_eq!(reply.type_().as_str(), "(u(ia{sv}av))");
        assert_eq!(reply.child_value(0).get::<u32>(), Some(3));
    }

    #[test]
    fn the_pause_item_names_what_it_will_do() {
        let label = |paused| menu_items(paused)[1].1;
        assert_eq!(label(false), Some("Pause dictation"));
        assert_eq!(label(true), Some("Resume dictation"));
    }

    #[test]
    fn a_click_runs_its_command_and_a_separator_does_nothing() {
        let items = menu_items(false);
        let ran = Cell::new(None);
        assert!(menu_call("Event", &click(1), &items, 1, &|c| ran.set(Some(c))).is_none());
        assert_eq!(ran.get(), Some(MenuCommand::TextTrace));
        menu_call("Event", &click(2), &items, 1, &|c| ran.set(Some(c)));
        assert_eq!(ran.get(), Some(MenuCommand::TogglePause));
        ran.set(None);
        menu_call("Event", &click(3), &items, 1, &|c| ran.set(Some(c)));
        assert_eq!(ran.get(), None);
    }

    #[test]
    fn every_property_has_its_declared_type() {
        let declared: Vec<(&str, &str)> = ITEM_XML
            .lines()
            .filter_map(|line| {
                let rest = line.trim().strip_prefix("<property name=\"")?;
                let (name, rest) = rest.split_once('"')?;
                let signature = rest.split_once("type=\"")?.1.split_once('"')?.0;
                Some((name, signature))
            })
            .collect();
        assert_eq!(declared.len(), 16);
        let icon = pixmaps(IconState::Ready);
        for (name, signature) in &declared {
            assert_eq!(
                item_property(name, &icon, "Ready").type_().as_str(),
                *signature,
                "{name}"
            );
        }
    }

    #[test]
    fn the_png_icons_load_and_recording_is_tinted_red() {
        for state in [IconState::Ready, IconState::Muted, IconState::Recording] {
            let icon = pixmaps(state);
            assert_eq!(icon.len(), SIZES.len());
            for (w, h, bytes) in &icon {
                assert_eq!(bytes.len(), (w * h * 4) as usize);
            }
        }
        // The brightest opaque pixel shows the tint.
        let brightest = |state| {
            let (_, _, bytes) = pixmaps(state).pop().unwrap();
            bytes
                .chunks(4)
                .filter(|p| p[0] == 255)
                .map(|p| [p[1], p[2], p[3]])
                .max_by_key(|p| u32::from(p[0]) + u32::from(p[1]) + u32::from(p[2]))
                .unwrap()
        };
        // The PNG's own color shows when ready; recording multiplies it by red.
        let white = brightest(IconState::Ready);
        let red = [229u16, 72, 77];
        let expected: Vec<u8> = (0..3)
            .map(|i| (u16::from(white[i]) * red[i] / 255) as u8)
            .collect();
        assert_eq!(brightest(IconState::Recording).to_vec(), expected);
        // The two PNGs differ (the muted one has the slash).
        assert_ne!(pixmaps(IconState::Ready), pixmaps(IconState::Muted));
    }
}
