//! Minimal XGrabKey + wait_for_event, to separate "the grab does not deliver"
//! from "our event loop drops it".
//!
//! Usage: grab-test [ctrl+alt+v]

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{ConnectionExt as _, GrabMode, ModMask};
use x11rb::protocol::Event;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let specs: Vec<String> = {
        let a: Vec<String> = std::env::args().skip(1).collect();
        if a.is_empty() { vec!["ctrl+alt+v".into()] } else { a }
    };
    let (conn, screen_num) = x11rb::connect(None)?;
    let root = conn.setup().roots[screen_num].root;

    let setup = conn.setup();
    let min = setup.min_keycode;
    let map = conn.get_keyboard_mapping(min, setup.max_keycode - min + 1)?.reply()?;
    let per = map.keysyms_per_keycode as usize;

    for spec in &specs {
        let mut mods = 0u16;
        let mut keycode = 0u8;
        for part in spec.split('+').map(str::trim) {
            match part.to_ascii_lowercase().as_str() {
                "super" | "meta" | "win" => mods |= ModMask::M4.bits(),
                "ctrl" | "control" => mods |= ModMask::CONTROL.bits(),
                "alt" => mods |= ModMask::M1.bits(),
                "shift" => mods |= ModMask::SHIFT.bits(),
                k => {
                    let sym = k.chars().next().unwrap() as u32;
                    keycode = map
                        .keysyms
                        .chunks(per)
                        .position(|c| c.contains(&sym))
                        .map(|i| min + i as u8)
                        .ok_or("no keycode")?;
                }
            }
        }

        println!("grabbing {spec}: keycode={keycode} mods=0x{mods:x} on root 0x{root:x}");
        let locks = [0u16, ModMask::LOCK.bits(), ModMask::M2.bits(),
                     ModMask::LOCK.bits() | ModMask::M2.bits()];
        for extra in locks {
            let combo = ModMask::from(mods | extra);
            match conn
                .grab_key(false, root, combo, keycode, GrabMode::ASYNC, GrabMode::ASYNC)?
                .check()
            {
                Ok(()) => println!("  grabbed with extra mods 0x{extra:x}"),
                Err(e) => println!("  FAILED with extra mods 0x{extra:x}: {e}"),
            }
        }
    }
    conn.flush()?;

    let secs: u64 = std::env::var("GRAB_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(10);
    println!("waiting for events ({secs}s)...");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    while std::time::Instant::now() < deadline {
        match conn.poll_for_event()? {
            Some(Event::KeyPress(e)) => println!("  KeyPress   detail={} state=0x{:x}", e.detail, u16::from(e.state)),
            Some(Event::KeyRelease(e)) => println!("  KeyRelease detail={}", e.detail),
            Some(other) => println!("  other: {other:?}"),
            None => std::thread::sleep(std::time::Duration::from_millis(5)),
        }
    }
    println!("done");
    Ok(())
}
