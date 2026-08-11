//! Send a synthetic Super+V via XTEST, to test the daemon's key grab without a
//! human at the keyboard.

use x11rb::connection::Connection;
use x11rb::protocol::xproto::ConnectionExt as _;
use x11rb::protocol::xtest::ConnectionExt as _;

const KEY_PRESS: u8 = 2;
const KEY_RELEASE: u8 = 3;
const KEYSYM_V: u32 = 0x0076;
const KEYSYM_SUPER_L: u32 = 0xffeb;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (conn, screen_num) = x11rb::connect(None)?;
    let root = conn.setup().roots[screen_num].root;

    let setup = conn.setup();
    let min = setup.min_keycode;
    let map = conn.get_keyboard_mapping(min, setup.max_keycode - min + 1)?.reply()?;
    let per = map.keysyms_per_keycode as usize;
    let find = |sym: u32| -> Option<u8> {
        map.keysyms
            .chunks(per)
            .position(|c| c.contains(&sym))
            .map(|i| min + i as u8)
    };

    let v = find(KEYSYM_V).ok_or("no keycode for v")?;
    let sup = find(KEYSYM_SUPER_L).ok_or("no keycode for Super_L")?;

    // Which modifier index actually carries Super on this keymap? The grab has
    // to name the right one, and it is not guaranteed to be mod4.
    let modmap = conn.get_modifier_mapping()?.reply()?;
    let per_mod = modmap.keycodes_per_modifier() as usize;
    const NAMES: [&str; 8] =
        ["Shift", "Lock", "Control", "Mod1", "Mod2", "Mod3", "Mod4", "Mod5"];
    for (i, chunk) in modmap.keycodes.chunks(per_mod).enumerate().take(8) {
        let codes: Vec<u8> = chunk.iter().copied().filter(|c| *c != 0).collect();
        let marker = if codes.contains(&sup) { "  <-- Super_L is here" } else { "" };
        println!("  {:<8} {:?}{}", NAMES[i], codes, marker);
    }

    // Optional arg: a combo like "ctrl+alt+v". Defaults to super+v.
    let spec = std::env::args().nth(1).unwrap_or_else(|| "super+v".into());
    let mut mods: Vec<u8> = Vec::new();
    let mut key = v;
    for part in spec.split('+').map(str::trim) {
        match part.to_ascii_lowercase().as_str() {
            "super" | "meta" | "win" => mods.push(sup),
            "ctrl" | "control" => mods.push(find(0xffe3).ok_or("no Control_L")?),
            "alt" => mods.push(find(0xffe9).ok_or("no Alt_L")?),
            "shift" => mods.push(find(0xffe1).ok_or("no Shift_L")?),
            k => {
                let c = k.chars().next().ok_or("empty key")?;
                key = find(c as u32).ok_or("no keycode for key")?;
            }
        }
    }

    // Press each modifier separately and confirm the server registered it.
    // A passive grab only matches when the state is exactly right at key-down,
    // so a modifier that silently fails to latch invalidates the whole test.
    for m in &mods {
        conn.xtest_fake_input(KEY_PRESS, *m, 0, root, 0, 0, 0)?;
        conn.flush()?;
        std::thread::sleep(std::time::Duration::from_millis(40));
        let st = conn.query_pointer(root)?.reply()?.mask;
        println!("  after pressing keycode {m}: state=0x{:x}", u16::from(st));
    }

    let state = conn.query_pointer(root)?.reply()?.mask;
    println!("  modifier state before key press: 0x{:x}", u16::from(state));

    conn.xtest_fake_input(KEY_PRESS, key, 0, root, 0, 0, 0)?;
    conn.flush()?;
    std::thread::sleep(std::time::Duration::from_millis(30));
    conn.xtest_fake_input(KEY_RELEASE, key, 0, root, 0, 0, 0)?;
    for m in mods.iter().rev() {
        conn.xtest_fake_input(KEY_RELEASE, *m, 0, root, 0, 0, 0)?;
    }
    conn.flush()?;
    println!("sent {spec} (mods={mods:?} key={key})");
    Ok(())
}
