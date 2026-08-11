//! Phase 0, Spike A — event-driven X11 clipboard capture.
//!
//! Proves the three claims the plan rests on:
//!   1. XFIXES gives us clipboard-change *events*, so idle CPU is genuinely 0%
//!      (no 500ms polling loop like most Linux clipboard managers).
//!   2. We can negotiate TARGETS first and fetch every flavor of a single copy.
//!   3. The password-manager MIME hint is visible *before* we read any data,
//!      so secrets can be dropped without ever touching them.
//!
//! Run it, then copy things. Ctrl-C to stop.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use x11rb::connection::Connection;
use x11rb::protocol::xfixes::{self, ConnectionExt as _, SelectionEventMask};
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ConnectionExt as _, CreateWindowAux, EventMask, Property, WindowClass,
};
use x11rb::protocol::Event;
use x11rb::rust_connection::RustConnection;
use x11rb::COPY_DEPTH_FROM_PARENT;

/// Flavors we ask for, best first. The first hit decides how the item renders;
/// the real daemon stores *all* of them against one item.
const PREFERRED: &[&str] = &[
    "image/png",
    "image/jpeg",
    "text/html",
    "text/uri-list",
    "UTF8_STRING",
    "STRING",
    "TEXT",
];

/// Set by KeePassXC, Bitwarden, 1Password, Firefox and Chromium on secrets.
const PASSWORD_HINT: &str = "x-kde-passwordManagerHint";
/// Our own paste marker — the loop guard.
const SELF_MARKER: &str = "application/x-clipd-serial";

const TEXT_CAP: usize = 10 * 1024 * 1024;
const IMAGE_CAP: usize = 50 * 1024 * 1024;

struct Atoms {
    clipboard: Atom,
    targets: Atom,
    incr: Atom,
    prop: Atom,
}

fn intern(conn: &RustConnection, name: &str) -> Result<Atom, Box<dyn std::error::Error>> {
    Ok(conn.intern_atom(false, name.as_bytes())?.reply()?.atom)
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (conn, screen_num) = x11rb::connect(None)?;
    let screen = &conn.setup().roots[screen_num];
    let root = screen.root;

    // XFIXES is what makes this event-driven rather than polled.
    let ver = conn.xfixes_query_version(5, 0)?.reply()?;
    println!("XFIXES {}.{}", ver.major_version, ver.minor_version);

    let atoms = Atoms {
        clipboard: intern(&conn, "CLIPBOARD")?,
        targets: intern(&conn, "TARGETS")?,
        incr: intern(&conn, "INCR")?,
        prop: intern(&conn, "CLIPD_SEL")?,
    };

    // An unmapped window is enough to be a selection requestor.
    let win = conn.generate_id()?;
    conn.create_window(
        COPY_DEPTH_FROM_PARENT,
        win,
        root,
        0,
        0,
        1,
        1,
        0,
        WindowClass::INPUT_OUTPUT,
        screen.root_visual,
        &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE),
    )?;

    xfixes::select_selection_input(
        &conn,
        root,
        atoms.clipboard,
        SelectionEventMask::SET_SELECTION_OWNER,
    )?;
    conn.flush()?;

    println!("watching CLIPBOARD — copy something (Ctrl-C to stop)\n");

    let mut seen: HashMap<u64, u32> = HashMap::new();
    let mut last: Option<(u64, Instant)> = None;

    loop {
        let event = conn.wait_for_event()?;
        let Event::XfixesSelectionNotify(ev) = event else {
            continue;
        };
        if ev.owner == x11rb::NONE {
            println!("-- clipboard cleared");
            continue;
        }
        if ev.owner == win {
            continue; // our own write
        }

        let t0 = Instant::now();

        // Step 1: what is on offer? We decide *before* reading any payload.
        let targets_raw = match fetch(&conn, win, &atoms, atoms.targets)? {
            Fetched::Data(_, d) => d,
            Fetched::Refused => {
                println!("-- owner 0x{:x} refused TARGETS", ev.owner);
                continue;
            }
            Fetched::Timeout => {
                println!("-- owner 0x{:x} vanished before answering TARGETS", ev.owner);
                continue;
            }
        };
        let target_atoms: Vec<Atom> = targets_raw
            .chunks_exact(4)
            .map(|c| u32::from_ne_bytes([c[0], c[1], c[2], c[3]]))
            .collect();

        let mut names: Vec<(Atom, String)> = Vec::with_capacity(target_atoms.len());
        for a in &target_atoms {
            let name = conn
                .get_atom_name(*a)?
                .reply()
                .map(|r| String::from_utf8_lossy(&r.name).into_owned())
                .unwrap_or_default();
            names.push((*a, name));
        }
        let t_targets = t0.elapsed();

        if names.iter().any(|(_, n)| n == SELF_MARKER) {
            println!("-- skipped: our own paste (loop guard held)");
            continue;
        }
        if names.iter().any(|(_, n)| n == PASSWORD_HINT) {
            println!(
                "-- SKIPPED SECRET: offer advertises {PASSWORD_HINT}; no payload read \
                 ({} flavors, decided in {:.1}ms)",
                names.len(),
                t_targets.as_secs_f64() * 1000.0
            );
            continue;
        }

        // Step 2: fetch the best flavor.
        let chosen = PREFERRED
            .iter()
            .find_map(|want| names.iter().find(|(_, n)| n == want).cloned());
        let Some((atom, name)) = chosen else {
            println!(
                "-- no usable flavor among: {}",
                names
                    .iter()
                    .map(|(_, n)| n.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            continue;
        };

        let (_ty, data) = match fetch(&conn, win, &atoms, atom)? {
            Fetched::Data(t, d) => (t, d),
            Fetched::Refused => {
                println!("-- {name}: owner refused the payload");
                continue;
            }
            Fetched::Timeout => {
                println!("-- {name}: owner vanished mid-transfer");
                continue;
            }
        };
        let elapsed = t0.elapsed();

        let cap = if name.starts_with("image/") { IMAGE_CAP } else { TEXT_CAP };
        if data.len() > cap {
            println!("-- {name}: {} bytes exceeds cap, dropped", data.len());
            continue;
        }

        let hash = fnv1a(&data);

        // GNOME Shell re-takes CLIPBOARD ownership when the owning app *exits*,
        // re-offering the same bytes with our custom targets stripped — which
        // defeats the marker loop-guard on its own. That hand-off can happen
        // seconds or minutes after the copy, so a time window cannot catch it.
        //
        // The time-independent rule: if the incoming content is byte-identical
        // to what is already at the head of the history, this is not a new copy.
        // The head *is* that content already, so there is nothing to record.
        if last.map(|(h, _)| h) == Some(hash) {
            let age = last.map(|(_, t): (u64, Instant)| t.elapsed()).unwrap_or_default();
            println!(
                "-- ignored re-offer of current head ({:.0}ms later, targets stripped: {})",
                age.as_secs_f64() * 1000.0,
                names.len()
            );
            continue;
        }
        last = Some((hash, Instant::now()));

        // Cheap stand-in for blake3 dedupe — proves the "one row, use_count++" path.
        let count = seen.entry(hash).or_insert(0);
        *count += 1;

        let preview = if name.starts_with("image/") {
            format!("<{} bytes of {}>", data.len(), name)
        } else {
            let s = String::from_utf8_lossy(&data);
            let s = s.trim().replace('\n', "⏎");
            s.chars().take(60).collect()
        };

        println!(
            "[{:>6.1}ms] {:<14} {:>9} B  flavors={:<2} {}  \"{}\"",
            elapsed.as_secs_f64() * 1000.0,
            name,
            data.len(),
            names.len(),
            if *count > 1 {
                format!("DEDUPE use_count={count}")
            } else {
                "new".into()
            },
            preview
        );
        println!(
            "           targets: {}",
            names
                .iter()
                .map(|(_, n)| n.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        );
        println!(
            "           (TARGETS negotiated in {:.1}ms, payload in {:.1}ms)",
            t_targets.as_secs_f64() * 1000.0,
            (elapsed - t_targets).as_secs_f64() * 1000.0
        );
    }
}

enum Fetched {
    Data(Atom, Vec<u8>),
    /// Owner explicitly declined this target (property == None).
    Refused,
    /// Owner never answered — usually means it exited mid-transfer.
    Timeout,
}

/// ConvertSelection + wait for the SelectionNotify that answers it.
/// Handles the INCR protocol, which any payload over ~256KB will use.
fn fetch(
    conn: &RustConnection,
    win: u32,
    atoms: &Atoms,
    target: Atom,
) -> Result<Fetched, Box<dyn std::error::Error>> {
    conn.delete_property(win, atoms.prop)?;
    conn.convert_selection(win, atoms.clipboard, target, atoms.prop, x11rb::CURRENT_TIME)?;
    conn.flush()?;

    let deadline = Instant::now() + Duration::from_millis(3000);
    while Instant::now() < deadline {
        let Some(event) = conn.poll_for_event()? else {
            std::thread::sleep(Duration::from_micros(200));
            continue;
        };
        let Event::SelectionNotify(sn) = event else { continue };
        if sn.property == x11rb::NONE {
            return Ok(Fetched::Refused);
        }

        // Peek at the type without consuming, so we can spot INCR.
        let probe = conn
            .get_property(false, win, atoms.prop, AtomEnum::ANY, 0, 0)?
            .reply()?;

        if probe.type_ == atoms.incr {
            // INCR: deleting the property tells the owner to send the first
            // chunk; each PropertyNotify(NewValue) carries one more. A
            // zero-length chunk ends the transfer.
            conn.delete_property(win, atoms.prop)?;
            conn.flush()?;
            let mut buf = Vec::new();
            let mut ty = AtomEnum::NONE.into();
            let incr_deadline = Instant::now() + Duration::from_millis(10_000);
            while Instant::now() < incr_deadline {
                let Some(ev) = conn.poll_for_event()? else {
                    std::thread::sleep(Duration::from_micros(200));
                    continue;
                };
                let Event::PropertyNotify(pn) = ev else { continue };
                if pn.window != win || pn.atom != atoms.prop || pn.state != Property::NEW_VALUE {
                    continue;
                }
                let head = conn
                    .get_property(false, win, atoms.prop, AtomEnum::ANY, 0, 0)?
                    .reply()?;
                let words = (head.bytes_after + 3) / 4;
                let chunk = conn
                    .get_property(true, win, atoms.prop, AtomEnum::ANY, 0, words.max(1))?
                    .reply()?;
                conn.flush()?;
                if chunk.value.is_empty() {
                    return Ok(Fetched::Data(ty, buf));
                }
                ty = chunk.type_;
                buf.extend_from_slice(&chunk.value);
                if buf.len() > IMAGE_CAP {
                    return Ok(Fetched::Data(ty, buf)); // caller enforces the cap
                }
            }
            return Ok(Fetched::Timeout);
        }

        // Ordinary transfer: probe told us the length, now take it all.
        let words = (probe.bytes_after + 3) / 4;
        let full = conn
            .get_property(true, win, atoms.prop, AtomEnum::ANY, 0, words.max(1))?
            .reply()?;
        return Ok(Fetched::Data(full.type_, full.value));
    }
    Ok(Fetched::Timeout)
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}
