//! Does an **XI2** passive grab receive keys that a core `XGrabKey` did not?
//!
//! Background: core `GrabKey` for Super+V registered successfully under GNOME
//! and then delivered nothing. The hypothesis is that Mutter's XI2 grabs shadow
//! core grabs. If so, grabbing the way Mutter does should work — and it needs
//! no configuration file, so it cannot disturb any existing shortcut.
//!
//! This also settles a second question: whether XTEST-synthesised keys trigger
//! another client's passive grab at all. If XI2 sees the synthetic key that the
//! core grab missed, XTEST is a valid probe and core grabs really are shadowed.
//!
//! Usage: xi-grab-test [super+shift+v ...]

use x11rb::connection::Connection;
use x11rb::protocol::xinput::{
    ConnectionExt as _, Device, GrabMode22, GrabOwner, GrabType, XIEventMask,
};
use x11rb::protocol::xproto::{ConnectionExt as _, ModMask};
use x11rb::protocol::Event;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let specs: Vec<String> = {
        let a: Vec<String> = std::env::args().skip(1).collect();
        if a.is_empty() { vec!["super+shift+v".into()] } else { a }
    };

    let (conn, screen_num) = x11rb::connect(None)?;
    let root = conn.setup().roots[screen_num].root;

    let ver = conn.xinput_xi_query_version(2, 3)?.reply()?;
    println!("XInput {}.{}", ver.major_version, ver.minor_version);
    if ver.major_version < 2 {
        return Err("XInput 2 unavailable".into());
    }

    let setup = conn.setup();
    let min = setup.min_keycode;
    let map = conn.get_keyboard_mapping(min, setup.max_keycode - min + 1)?.reply()?;
    let per = map.keysyms_per_keycode as usize;

    for spec in &specs {
        let mut mods = 0u32;
        let mut keycode = 0u8;
        for part in spec.split('+').map(str::trim) {
            match part.to_ascii_lowercase().as_str() {
                "super" | "meta" | "win" => mods |= u32::from(ModMask::M4.bits()),
                "ctrl" | "control" => mods |= u32::from(ModMask::CONTROL.bits()),
                "alt" => mods |= u32::from(ModMask::M1.bits()),
                "shift" => mods |= u32::from(ModMask::SHIFT.bits()),
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

        // Cover the lock modifiers, exactly as the core-grab path had to.
        let lock = u32::from(ModMask::LOCK.bits());
        let num = u32::from(ModMask::M2.bits());
        let modifiers: Vec<u32> = vec![mods, mods | lock, mods | num, mods | lock | num];

        let mask = u32::from(XIEventMask::KEY_PRESS);

        let reply = conn
            .xinput_xi_passive_grab_device(
                x11rb::CURRENT_TIME,
                root,
                x11rb::NONE,               // cursor
                keycode as u32,            // detail
                Device::ALL_MASTER,        // deviceid
                GrabType::KEYCODE,
                GrabMode22::ASYNC,         // grab_mode (this device)
                x11rb::protocol::xproto::GrabMode::ASYNC, // paired_device_mode
                GrabOwner::NO_OWNER,       // owner_events
                &[mask],
                &modifiers,
            )?
            .reply()?;

        // The reply lists only the variants that FAILED.
        println!(
            "XI2 grab {spec}: keycode={keycode} base mods=0x{mods:x}",
        );
        println!("  requested variants: {:?}", modifiers.iter().map(|m| format!("0x{m:x}")).collect::<Vec<_>>());
        if reply.modifiers.is_empty() {
            println!("  all variants grabbed successfully");
        } else {
            for m in &reply.modifiers {
                println!("  REJECTED 0x{:x} (status {:?})", m.modifiers, m.status);
            }
        }
    }
    conn.flush()?;

    let secs: u64 = std::env::var("GRAB_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(12);
    println!("waiting {secs}s for XI2 key events...");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    let mut hits = 0;
    while std::time::Instant::now() < deadline {
        match conn.poll_for_event()? {
            Some(Event::XinputKeyPress(e)) => {
                hits += 1;
                println!("  XI2 KeyPress detail={} mods=0x{:x}", e.detail, e.mods.effective);
            }
            Some(_) => {}
            None => std::thread::sleep(std::time::Duration::from_millis(5)),
        }
    }
    println!("done — {hits} XI2 key events received");

    // Release the grabs so nothing is left held after we exit.
    for spec in &specs {
        let _ = spec;
    }
    Ok(())
}
