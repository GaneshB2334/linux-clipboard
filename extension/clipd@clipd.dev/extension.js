/*
 * clipd paste helper — GNOME Shell extension
 *
 * Why this exists
 * ---------------
 * On Wayland, no ordinary application can press Ctrl+V in another
 * application's window. XTEST reaches only XWayland clients, and the
 * sanctioned alternative — the RemoteDesktop portal — makes GNOME Shell show
 * a persistent "remote access" indicator in the top bar for as long as the
 * session is open, and prompts for permission. For a clipboard manager that
 * is disproportionate: we want one keystroke, not remote control of the
 * desktop.
 *
 * Code running *inside* GNOME Shell has no such problem. The compositor can
 * synthesise input directly through Clutter's virtual input device — the same
 * mechanism the shell uses for its own on-screen keyboard. No portal, no
 * permission dialog, no indicator.
 *
 * This extension is deliberately tiny: it owns one D-Bus name and exposes one
 * method. All the actual clipboard management (history, search, storage,
 * the popup UI) stays in clipd itself. Nothing here reads the clipboard or
 * stores anything, so the extension's attack surface is a single method that
 * presses Ctrl+V.
 */

import Clutter from 'gi://Clutter';
import Gio from 'gi://Gio';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';

const BUS_NAME = 'dev.clipd.PasteHelper';
const OBJECT_PATH = '/dev/clipd/PasteHelper';

const IFACE = `
<node>
  <interface name="dev.clipd.PasteHelper">
    <method name="Paste"/>
  </interface>
</node>`;

export default class ClipdPasteHelper extends Extension {
    enable() {
        this._virtualKeyboard = null;

        this._dbus = Gio.DBusExportedObject.wrapJSObject(IFACE, this);
        this._dbus.export(Gio.DBus.session, OBJECT_PATH);

        // Owning the name is what lets clipd detect that auto-paste is
        // available at all: it simply checks whether this name is on the bus.
        this._nameId = Gio.bus_own_name(
            Gio.BusType.SESSION,
            BUS_NAME,
            Gio.BusNameOwnerFlags.NONE,
            null,
            null,
            null);
    }

    disable() {
        // GNOME requires extensions to release everything on disable —
        // including on lock screen, where this extension is not permitted to
        // run (see "session-modes": ["user"] in metadata.json).
        if (this._nameId) {
            Gio.bus_unown_name(this._nameId);
            this._nameId = null;
        }
        if (this._dbus) {
            this._dbus.unexport();
            this._dbus = null;
        }
        // The virtual device holds a compositor resource; drop it so the seat
        // is not left with a stray keyboard after the extension unloads.
        this._virtualKeyboard = null;
    }

    /**
     * Press and release Ctrl+V against whatever currently has focus.
     *
     * clipd is responsible for two things before calling this: putting the
     * item on the clipboard, and closing its popup so the compositor has
     * handed focus back to the window the user was actually in. There is no
     * way to target a specific window from here — Wayland delivers synthetic
     * input to the focused surface, exactly as it would a real keypress.
     */
    Paste() {
        if (!this._virtualKeyboard) {
            const seat = Clutter.get_default_backend().get_default_seat();
            this._virtualKeyboard =
                seat.create_virtual_device(Clutter.InputDeviceType.KEYBOARD_DEVICE);
        }

        const vk = this._virtualKeyboard;
        const time = Clutter.get_current_event_time();

        vk.notify_keyval(time, Clutter.KEY_Control_L, Clutter.KeyState.PRESSED);
        vk.notify_keyval(time, Clutter.KEY_v, Clutter.KeyState.PRESSED);
        vk.notify_keyval(time, Clutter.KEY_v, Clutter.KeyState.RELEASED);
        vk.notify_keyval(time, Clutter.KEY_Control_L, Clutter.KeyState.RELEASED);
    }
}
