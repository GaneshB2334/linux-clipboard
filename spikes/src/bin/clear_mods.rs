//! Release every modifier key and mouse button that is currently held.
//!
//! XTEST tests can leave a modifier latched — if a grab swallows the synthetic
//! key-release, the server still believes the key is down, and the user's
//! keyboard starts behaving strangely. This forces everything back up.

use x11rb::connection::Connection;
use x11rb::protocol::xproto::ConnectionExt as _;
use x11rb::protocol::xtest::ConnectionExt as _;

const KEY_RELEASE: u8 = 3;
const BUTTON_RELEASE: u8 = 5;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let (conn, screen_num) = x11rb::connect(None)?;
    let root = conn.setup().roots[screen_num].root;

    let before = conn.query_pointer(root)?.reply()?.mask;
    println!("state before: 0x{:x}", u16::from(before));

    // Release every keycode that is mapped to a modifier.
    let modmap = conn.get_modifier_mapping()?.reply()?;
    for &kc in modmap.keycodes.iter().filter(|k| **k != 0) {
        conn.xtest_fake_input(KEY_RELEASE, kc, 0, root, 0, 0, 0)?;
    }
    // And every mouse button.
    for button in 1..=5u8 {
        conn.xtest_fake_input(BUTTON_RELEASE, button, 0, root, 0, 0, 0)?;
    }
    conn.flush()?;
    std::thread::sleep(std::time::Duration::from_millis(50));

    let after = conn.query_pointer(root)?.reply()?.mask;
    println!("state after:  0x{:x}", u16::from(after));
    // 0x10 is Num Lock, which is a genuine latched state, not stuck input.
    let stuck = u16::from(after) & !0x10;
    if stuck == 0 {
        println!("clean (only Num Lock remains, which is normal)");
    } else {
        println!("WARNING: 0x{stuck:x} still held");
    }
    Ok(())
}
