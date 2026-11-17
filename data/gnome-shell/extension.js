import Clutter from 'gi://Clutter';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Meta from 'gi://Meta';
import Shell from 'gi://Shell';
import Pango from 'gi://Pango';
import St from 'gi://St';

import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';

const APP_ID = 'io.github.franklima260.LiveTyper';
const PAD_PATH = '/io/github/franklima260/LiveTyper/ScratchPad';
const PAD_INTERFACE = `${APP_ID}.ScratchPad`;
const MAX_DRAFT_HEIGHT = 240;
const PAD_XML = `<node><interface name="${PAD_INTERFACE}">
    <method name="GetTalkControl">
        <arg type="b" direction="out" />
    </method>
    <method name="GetPasteTarget">
        <arg type="u" direction="out" />
        <arg type="b" direction="out" />
    </method>
</interface></node>`;

export default class LiveTyperScratchPad extends Extension {
    enable() {
        this._panel = new St.BoxLayout({
            style_class: 'livetyper-panel',
            visible: false,
            reactive: false,
            can_focus: false,
        });
        const text = new St.BoxLayout({
            orientation: Clutter.Orientation.VERTICAL,
            style_class: 'livetyper-text',
            x_expand: true,
        });
        this._status = new St.Label({style_class: 'livetyper-status'});
        this._status.clutter_text.line_wrap = true;
        this._draft = new St.Label({style_class: 'livetyper-draft', x_expand: true});
        this._draft.clutter_text.line_wrap = true;
        this._draft.clutter_text.line_wrap_mode = Pango.WrapMode.WORD_CHAR;
        this._draft.clutter_text.ellipsize = Pango.EllipsizeMode.NONE;

        this._scroll = new St.ScrollView({
            style_class: 'livetyper-scroll',
            reactive: false,
            enable_mouse_scrolling: false,
            hscrollbar_policy: St.PolicyType.NEVER,
            vscrollbar_policy: St.PolicyType.EXTERNAL,
        });
        const draftBox = new St.BoxLayout({orientation: Clutter.Orientation.VERTICAL});
        draftBox.add_child(this._draft);
        this._scroll.set_child(draftBox);
        const adjustment = this._scroll.vadjustment;
        adjustment.connect('notify::upper', () => this._scrollToEnd());
        adjustment.connect('notify::page-size', () => this._scrollToEnd());
        text.add_child(this._status);
        text.add_child(this._scroll);
        this._panel.add_child(text);

        this._meter = new St.DrawingArea({width: 16, height: 40, y_align: Clutter.ActorAlign.START});
        this._meter.connect('repaint', () => this._drawMeter());
        this._panel.add_child(this._meter);
        this._segments = [];
        this._width = 0;
        this._escapeActions = new Set();
        this._cancelEnabled = false;
        this._accelerator = global.display.connect('accelerator-activated', (_display, action) => {
            if (this._escapeActions.has(action))
                Gio.DBus.session.emit_signal(this._appOwner, PAD_PATH, PAD_INTERFACE, 'Cancel', null);
        });

        Main.layoutManager.addTopChrome(this._panel);
        this._panel.connect('notify::height', () => this._position());
        this._monitors = Main.layoutManager.connect('monitors-changed', () => this._position());
        this._workareas = global.display.connect('workareas-changed', () => this._position());
        this._sessionMode = Main.sessionMode.connect('updated', () => {
            if (Main.sessionMode.isLocked) {
                this._setEscape(false);
                this._panel.hide();
            }
        });
        this._updates = Gio.DBus.session.signal_subscribe(
            APP_ID, PAD_INTERFACE, 'Changed', PAD_PATH, null, Gio.DBusSignalFlags.NONE,
            (_bus, sender, _path, _interface, _signal, parameters) => {
                this._appOwner = sender;
                this._update(...parameters.deepUnpack());
            });
        this._dbus = Gio.DBusExportedObject.wrapJSObject(PAD_XML, this);
        this._dbus.export(Gio.DBus.session, PAD_PATH);
        this._owner = Gio.bus_watch_name_on_connection(
            Gio.DBus.session, APP_ID, Gio.BusNameWatcherFlags.NONE, null,
            () => {
                this._setEscape(false);
                this._panel.hide();
                this._appOwner = null;
            });
    }

    disable() {
        this._dbus.unexport();
        this._setEscape(false);
        global.display.disconnect(this._accelerator);
        Gio.DBus.session.signal_unsubscribe(this._updates);
        Gio.bus_unwatch_name(this._owner);
        Main.sessionMode.disconnect(this._sessionMode);
        global.display.disconnect(this._workareas);
        Main.layoutManager.disconnect(this._monitors);
        Main.layoutManager.removeChrome(this._panel);
        this._panel.destroy();
        this._panel = null;
        this._status = null;
        this._draft = null;
        this._scroll = null;
        this._meter = null;
        this._segments = [];
    }

    GetTalkControlAsync(_parameters, invocation) {
        if (invocation.get_sender() !== this._appOwner || Main.sessionMode.isLocked) {
            invocation.return_dbus_error('org.freedesktop.DBus.Error.AccessDenied',
                'Only the active LiveTyper instance may query the Talk Key.');
            return;
        }
        const [, , modifiers] = global.get_pointer();
        invocation.return_value(new GLib.Variant('(b)', [
            Boolean(modifiers & Clutter.ModifierType.CONTROL_MASK),
        ]));
    }

    GetPasteTargetAsync(_parameters, invocation) {
        if (invocation.get_sender() !== this._appOwner) {
            invocation.return_dbus_error('org.freedesktop.DBus.Error.AccessDenied',
                'Only the active LiveTyper instance may query the paste target.');
            return;
        }
        const window = Main.sessionMode.isLocked ? null : global.display.get_focus_window();
        const [, , modifiers] = global.get_pointer();
        const blocksPaste = Clutter.ModifierType.SHIFT_MASK |
            Clutter.ModifierType.MOD1_MASK | Clutter.ModifierType.MOD4_MASK |
            Clutter.ModifierType.SUPER_MASK;
        invocation.return_value(new GLib.Variant('(ub)', [
            window?.get_stable_sequence() ?? 0,
            Boolean(modifiers & blocksPaste),
        ]));
    }

    _setEscape(enabled) {
        if (this._cancelEnabled === enabled)
            return;
        this._cancelEnabled = enabled;

        if (!enabled) {
            for (const action of this._escapeActions) {
                Main.wm.allowKeybinding(Meta.external_binding_name_for_action(action),
                    Shell.ActionMode.NONE);
                global.display.ungrab_accelerator(action);
            }
            this._escapeActions.clear();
            return;
        }

        // A held Talk Key can leave modifiers down. Include their subsets so
        // Escape keeps working while the user releases the chord.
        const [, , modifiers] = global.get_pointer();
        let prefixes = [''];
        for (const [mask, modifier] of [
            [Clutter.ModifierType.CONTROL_MASK, '<Control>'],
            [Clutter.ModifierType.SHIFT_MASK, '<Shift>'],
            [Clutter.ModifierType.MOD1_MASK, '<Alt>'],
            [Clutter.ModifierType.MOD4_MASK | Clutter.ModifierType.SUPER_MASK, '<Super>'],
        ]) {
            if (modifiers & mask)
                prefixes = prefixes.concat(prefixes.map(prefix => prefix + modifier));
        }
        for (const prefix of prefixes) {
            const action = global.display.grab_accelerator(prefix + 'Escape',
                Meta.KeyBindingFlags.IGNORE_AUTOREPEAT);
            if (action !== Meta.KeyBindingAction.NONE) {
                this._escapeActions.add(action);
                Main.wm.allowKeybinding(Meta.external_binding_name_for_action(action),
                    Shell.ActionMode.NORMAL);
            }
        }
    }

    _update(visible, status, draft, meterVisible, width, segments, cancelEnabled = false) {
        this._setEscape(cancelEnabled && !Main.sessionMode.isLocked);
        if (!visible || Main.sessionMode.isLocked) {
            this._panel.hide();
            return;
        }

        this._width = width;
        this._status.text = status;
        this._draft.clutter_text.set_markup(draft);
        this._scroll.visible = draft.length > 0;
        this._meter.visible = meterVisible;
        this._segments = segments;
        if (meterVisible)
            this._meter.queue_repaint();
        this._position();
        this._panel.show();
    }

    _position() {
        const monitor = Main.layoutManager.primaryMonitor;
        if (!monitor)
            return;
        const area = Main.layoutManager.getWorkAreaForMonitor(monitor.index);
        const width = Math.min(this._width, Math.max(1, area.width - 32));
        this._panel.width = width;
        if (this._scroll.visible) {
            const theme = this._panel.get_theme_node();
            const meterWidth = this._meter.visible ? this._meter.width + theme.get_length('spacing') : 0;
            const textWidth = Math.max(1, width - theme.get_horizontal_padding() - meterWidth);
            const [, draftHeight] = this._draft.get_preferred_height(textWidth);
            this._scroll.height = Math.min(MAX_DRAFT_HEIGHT, Math.ceil(draftHeight));
        }
        this._panel.set_position(
            Math.round(area.x + (area.width - width) / 2),
            Math.round(Math.max(area.y, area.y + area.height - 44 - this._panel.height)));
    }

    _scrollToEnd() {
        const adjustment = this._scroll.vadjustment;
        adjustment.value = Math.max(adjustment.lower, adjustment.upper - adjustment.page_size);
    }

    _drawMeter() {
        const [width, height] = this._meter.get_surface_size();
        const cr = this._meter.get_context();
        const gap = 1.5;
        const segmentHeight = (height + gap) / this._segments.length;
        this._segments.forEach(([lit, held, red, green, blue], index) => {
            const y = height - (index + 1) * segmentHeight + gap;
            cr.setSourceRGBA(red, green, blue, lit ? 1 : 0.18);
            cr.rectangle(0, y, width, segmentHeight - gap);
            cr.fill();
            if (held) {
                cr.setSourceRGB(0.95, 0.95, 0.95);
                cr.rectangle(0, y, width, 1);
                cr.fill();
            }
        });
        cr.$dispose();
    }
}
