//! Phase 0, Spike B — clipboard ownership + focus restore + XTEST paste.
//!
//! This is the spike that decides whether the product is possible at all.
//! It reproduces the full popup sequence:
//!
//!   1. remember _NET_ACTIVE_WINDOW  (the app you were typing in)
//!   2. map a normal window and take focus   (the popup opening)
//!   3. take CLIPBOARD ownership with the payload + our loop-guard marker
//!   4. hide, hand focus back to the remembered window, let it settle
//!   5. XTEST synthetic Ctrl+V
//!   6. serve the SelectionRequest the target app sends back
//!
//! Step 4 is where every Linux clipboard manager gets it wrong and pastes into
//! the wrong window. Step 3's marker is what stops us re-ingesting our own paste.
//!
//! Usage:  x11-paste "text to paste" [arm-delay-seconds]
//! Focus your target app during the arming countdown.

use std::time::{Duration, Instant};

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ClientMessageEvent, ConnectionExt as _, CreateWindowAux, EventMask, InputFocus,
    PropMode, SelectionNotifyEvent, StackMode, Time, WindowClass, CLIENT_MESSAGE_EVENT,
    SELECTION_NOTIFY_EVENT,
};
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;
use x11rb::COPY_DEPTH_FROM_PARENT;

const KEY_PRESS: u8 = 2;
const KEY_RELEASE: u8 = 3;
const KEYSYM_V: u32 = 0x0076;
const KEYSYM_CONTROL_L: u32 = 0xffe3;

/// Marker target carried on every write we make. The capture side sees this in
/// TARGETS and drops the event, so pasting never re-enters history.
const SELF_MARKER: &str = "application/x-clipd-serial";

fn intern(conn: &RustConnection, name: &str) -> Result<Atom, Box<dyn std::error::Error>> {
    Ok(conn.intern_atom(false, name.as_bytes())?.reply()?.atom)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // --set-only   own the clipboard and serve it; no focus games, no XTEST.
    //              (a test writer, so the capture spike has something to see)
    // --no-marker  omit the loop-guard target, so we look like a foreign app.
    let mut payload = String::from("hello from clipd");
    let mut set_only = false;
    let mut no_marker = false;
    let mut arm: u64 = 5;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--set-only" => set_only = true,
            "--no-marker" => no_marker = true,
            s if s.starts_with("--arm=") => arm = s[6..].parse().unwrap_or(5),
            s => payload = s.to_string(),
        }
    }

    let (conn, screen_num) = x11rb::connect(None)?;
    let screen = &conn.setup().roots[screen_num];
    let root = screen.root;

    let a_clipboard = intern(&conn, "CLIPBOARD")?;
    let a_targets = intern(&conn, "TARGETS")?;
    let a_utf8 = intern(&conn, "UTF8_STRING")?;
    let a_text = intern(&conn, "TEXT")?;
    let a_timestamp = intern(&conn, "TIMESTAMP")?;
    let a_marker = intern(&conn, SELF_MARKER)?;
    let a_active = intern(&conn, "_NET_ACTIVE_WINDOW")?;

    let mut target_win = x11rb::NONE;
    if !set_only {
        println!("Focus the app you want to paste into. Capturing in {arm}s...");
        for i in (1..=arm).rev() {
            println!("  {i}");
            std::thread::sleep(Duration::from_secs(1));
        }

        // ---- 1. remember where the user was --------------------------------
        target_win = conn
            .get_property(false, root, a_active, AtomEnum::WINDOW, 0, 1)?
            .reply()?
            .value32()
            .and_then(|mut v| v.next())
            .unwrap_or(x11rb::NONE);

        if target_win == x11rb::NONE {
            eprintln!("could not read _NET_ACTIVE_WINDOW — is a WM running?");
            return Ok(());
        }
        println!("\ntarget window: 0x{target_win:x} ({})", window_name(&conn, target_win));
    }

    let total = Instant::now();

    // ---- 2. the popup opens and steals focus ------------------------------
    let win = conn.generate_id()?;
    conn.create_window(
        COPY_DEPTH_FROM_PARENT,
        win,
        root,
        200,
        200,
        420,
        260,
        0,
        WindowClass::INPUT_OUTPUT,
        screen.root_visual,
        &CreateWindowAux::new()
            .event_mask(EventMask::PROPERTY_CHANGE | EventMask::STRUCTURE_NOTIFY)
            .background_pixel(screen.black_pixel),
    )?;
    if !set_only {
        conn.map_window(win)?;
        conn.configure_window(
            win,
            &x11rb::protocol::xproto::ConfigureWindowAux::new().stack_mode(StackMode::ABOVE),
        )?;
        conn.flush()?;

        // Wait for the window to actually be viewable before focusing it —
        // focusing an unmapped window is a BadMatch.
        let t_map = Instant::now();
        wait_for_map(&conn, win)?;
        conn.set_input_focus(InputFocus::PARENT, win, Time::CURRENT_TIME)?;
        conn.flush()?;
        println!("popup mapped + focused in {:.1}ms", t_map.elapsed().as_secs_f64() * 1000.0);

        std::thread::sleep(Duration::from_millis(400)); // "user reads the list"
    }

    // ---- 3. own the clipboard ---------------------------------------------
    conn.set_selection_owner(win, a_clipboard, Time::CURRENT_TIME)?;
    conn.flush()?;
    let owner = conn.get_selection_owner(a_clipboard)?.reply()?.owner;
    if owner != win {
        eprintln!("failed to take CLIPBOARD ownership (owner=0x{owner:x})");
        return Ok(());
    }
    println!("CLIPBOARD ownership acquired");

    if set_only {
        println!(
            "clipboard set to {:?}{} — serving for 120s",
            payload,
            if no_marker { " (no loop-guard marker)" } else { "" }
        );
        serve(&conn, &payload, Duration::from_secs(120), no_marker, ServeAtoms {
            targets: a_targets, timestamp: a_timestamp, utf8: a_utf8, text: a_text, marker: a_marker,
        })?;
        return Ok(());
    }

    // ---- 4. hide, restore focus, let it settle ----------------------------
    let t_restore = Instant::now();
    conn.unmap_window(win)?;
    conn.flush()?;

    // EWMH _NET_ACTIVE_WINDOW is the request the WM actually honours; a bare
    // SetInputFocus loses to GNOME's focus-stealing prevention on its own.
    let msg = ClientMessageEvent {
        response_type: CLIENT_MESSAGE_EVENT,
        format: 32,
        sequence: 0,
        window: target_win,
        type_: a_active,
        data: [2 /* source: pager */, Time::CURRENT_TIME.into(), win, 0, 0].into(),
    };
    conn.send_event(
        false,
        root,
        EventMask::SUBSTRUCTURE_NOTIFY | EventMask::SUBSTRUCTURE_REDIRECT,
        msg,
    )?;
    conn.set_input_focus(InputFocus::PARENT, target_win, Time::CURRENT_TIME)?;
    conn.flush()?;

    // Settle. Too short and the keystroke lands in the wrong window.
    std::thread::sleep(Duration::from_millis(30));

    let focused = conn.get_input_focus()?.reply()?.focus;
    let regained = focus_belongs_to(&conn, focused, target_win);
    println!(
        "focus restored in {:.1}ms -> 0x{focused:x} {}",
        t_restore.elapsed().as_secs_f64() * 1000.0,
        if regained { "MATCHES target ✓" } else { "MISMATCH ✗" }
    );

    // Refuse to inject into a window we did not intend. A synthetic Ctrl+V goes
    // to whatever holds focus, so a failed restore would paste into a random
    // document. The real daemon must have this guard too: leave the item on the
    // clipboard and let the user press Ctrl+V, rather than guess.
    if !regained {
        eprintln!(
            "\nABORTED: focus is 0x{focused:x}, expected 0x{target_win:x}. \
             Not injecting Ctrl+V.\nContent is on the clipboard — paste it manually."
        );
        serve(&conn, &payload, Duration::from_secs(20), false, ServeAtoms {
            targets: a_targets, timestamp: a_timestamp, utf8: a_utf8, text: a_text, marker: a_marker,
        })?;
        return Ok(());
    }

    // ---- 5. synthetic Ctrl+V ----------------------------------------------
    let kc_v = keycode_for(&conn, KEYSYM_V)?.ok_or("no keycode for 'v'")?;
    let kc_ctrl = keycode_for(&conn, KEYSYM_CONTROL_L)?.ok_or("no keycode for Control_L")?;

    let t_key = Instant::now();
    conn.xtest_fake_input(KEY_PRESS, kc_ctrl, 0, root, 0, 0, 0)?;
    conn.xtest_fake_input(KEY_PRESS, kc_v, 0, root, 0, 0, 0)?;
    conn.xtest_fake_input(KEY_RELEASE, kc_v, 0, root, 0, 0, 0)?;
    conn.xtest_fake_input(KEY_RELEASE, kc_ctrl, 0, root, 0, 0, 0)?;
    conn.flush()?;
    println!(
        "XTEST Ctrl+V sent in {:.2}ms (keycodes ctrl={kc_ctrl} v={kc_v})",
        t_key.elapsed().as_secs_f64() * 1000.0
    );
    println!(
        "\ntotal popup->pasted: {:.1}ms",
        total.elapsed().as_secs_f64() * 1000.0
    );

    // ---- 6. serve the paste -----------------------------------------------
    println!("serving SelectionRequests for 5s...\n");
    let served = serve(&conn, &payload, Duration::from_secs(5), false, ServeAtoms {
        targets: a_targets, timestamp: a_timestamp, utf8: a_utf8, text: a_text, marker: a_marker,
    })?;

    if served == 0 {
        println!("NO SelectionRequests received — the paste did not reach a target app.");
    } else {
        println!("\n{served} requests served — paste delivered.");
    }
    Ok(())
}

struct ServeAtoms {
    targets: Atom,
    timestamp: Atom,
    utf8: Atom,
    text: Atom,
    marker: Atom,
}

/// Act as CLIPBOARD owner: answer TARGETS/TIMESTAMP/UTF8_STRING requests until
/// the deadline. Returns how many requests were served.
fn serve(
    conn: &RustConnection,
    payload: &str,
    dur: Duration,
    no_marker: bool,
    a: ServeAtoms,
) -> Result<u32, Box<dyn std::error::Error>> {
    let deadline = Instant::now() + dur;
    let mut served = 0;
    while Instant::now() < deadline {
        let Some(event) = conn.poll_for_event()? else {
            std::thread::sleep(Duration::from_micros(500));
            continue;
        };
        match event {
            Event::SelectionRequest(req) => {
                // Obsolete clients send property=None and mean "use target".
                let prop = if req.property == x11rb::NONE { req.target } else { req.property };
                let mut ok = true;

                if req.target == a.targets {
                    let mut offer =
                        vec![a.targets, a.timestamp, a.utf8, a.text, AtomEnum::STRING.into()];
                    if !no_marker {
                        offer.push(a.marker);
                    }
                    conn.change_property32(
                        PropMode::REPLACE,
                        req.requestor,
                        prop,
                        AtomEnum::ATOM,
                        &offer,
                    )?;
                } else if req.target == a.timestamp {
                    conn.change_property32(
                        PropMode::REPLACE,
                        req.requestor,
                        prop,
                        AtomEnum::INTEGER,
                        &[0u32],
                    )?;
                } else if req.target == a.utf8
                    || req.target == a.text
                    || req.target == Atom::from(AtomEnum::STRING)
                {
                    conn.change_property8(
                        PropMode::REPLACE,
                        req.requestor,
                        prop,
                        req.target,
                        payload.as_bytes(),
                    )?;
                } else if req.target == a.marker && !no_marker {
                    conn.change_property8(PropMode::REPLACE, req.requestor, prop, req.target, b"1")?;
                } else {
                    ok = false;
                }

                let notify = SelectionNotifyEvent {
                    response_type: SELECTION_NOTIFY_EVENT,
                    sequence: 0,
                    time: req.time,
                    requestor: req.requestor,
                    selection: req.selection,
                    target: req.target,
                    property: if ok { prop } else { x11rb::NONE },
                };
                conn.send_event(false, req.requestor, EventMask::NO_EVENT, notify)?;
                conn.flush()?;

                served += 1;
                println!(
                    "  served {:<24} to 0x{:x} {}",
                    atom_name(conn, req.target),
                    req.requestor,
                    if ok { "✓" } else { "(refused)" }
                );
            }
            Event::SelectionClear(_) => {
                println!("  lost CLIPBOARD ownership (another app took it)");
                return Ok(served);
            }
            _ => {}
        }
    }
    Ok(served)
}

fn wait_for_map(conn: &RustConnection, win: u32) -> Result<(), Box<dyn std::error::Error>> {
    let deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < deadline {
        if let Some(Event::MapNotify(m)) = conn.poll_for_event()? {
            if m.window == win {
                return Ok(());
            }
        }
        if conn.get_window_attributes(win)?.reply()?.map_state
            == x11rb::protocol::xproto::MapState::VIEWABLE
        {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    Ok(())
}

/// GetInputFocus can report a child of the toplevel we asked for, so walk up.
fn focus_belongs_to(conn: &RustConnection, mut focus: u32, target: u32) -> bool {
    for _ in 0..8 {
        if focus == target {
            return true;
        }
        match conn.query_tree(focus).ok().and_then(|c| c.reply().ok()) {
            Some(tree) if tree.parent != x11rb::NONE && tree.parent != tree.root => {
                focus = tree.parent
            }
            _ => break,
        }
    }
    false
}

fn keycode_for(
    conn: &RustConnection,
    keysym: u32,
) -> Result<Option<u8>, Box<dyn std::error::Error>> {
    let setup = conn.setup();
    let min = setup.min_keycode;
    let max = setup.max_keycode;
    let count = max - min + 1;
    let map = conn.get_keyboard_mapping(min, count)?.reply()?;
    let per = map.keysyms_per_keycode as usize;
    for (i, chunk) in map.keysyms.chunks(per).enumerate() {
        if chunk.contains(&keysym) {
            return Ok(Some(min + i as u8));
        }
    }
    Ok(None)
}

fn atom_name(conn: &RustConnection, atom: Atom) -> String {
    conn.get_atom_name(atom)
        .ok()
        .and_then(|c| c.reply().ok())
        .map(|r| String::from_utf8_lossy(&r.name).into_owned())
        .unwrap_or_else(|| format!("atom#{atom}"))
}

fn window_name(conn: &RustConnection, win: u32) -> String {
    conn.get_property(false, win, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, 128)
        .ok()
        .and_then(|c| c.reply().ok())
        .map(|r| {
            String::from_utf8_lossy(&r.value)
                .split('\0')
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(".")
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "?".into())
}
