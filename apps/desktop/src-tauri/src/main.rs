// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Ganesh Bastapure

// Prevent an extra console window on Windows in release.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // On Wayland sessions clipd-desktop runs under XWayland (`GDK_BACKEND=x11`).
    // GTK3 defaults to `im-ibus`, which silently queues and drops every keystroke
    // (including arrow keys and Enter) if the X11 IBus socket is missing or stale.
    // GTK's built-in simple input context delivers all keystrokes directly.
    #[cfg(target_os = "linux")]
    if std::env::var_os("GTK_IM_MODULE").is_none() {
        std::env::set_var("GTK_IM_MODULE", "gtk-im-context-simple");
    }

    clipd_desktop_lib::run()
}
