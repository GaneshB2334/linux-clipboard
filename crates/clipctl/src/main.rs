//! SPDX-License-Identifier: GPL-3.0-or-later
//! Copyright (C) 2026 Ganesh Bastapure
//!
//! Tiny CLI for the daemon socket.
//!
//! On X11 the daemon grabs the hotkey itself, so this is mostly a debugging and
//! scripting tool. On Wayland it becomes load-bearing: compositors will only
//! *spawn a command* for a keybinding, and this binary is what they spawn — so
//! it stays free of heavy dependencies to keep process start off the critical
//! path.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;

use base64::Engine as _;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let request = match args.first().map(String::as_str) {
        Some("toggle") | None => json(r#"{"op":"toggle_popup"}"#),
        Some("list") => {
            let limit = args.get(1).and_then(|s| s.parse::<u32>().ok()).unwrap_or(20);
            format!(r#"{{"op":"recent","limit":{limit}}}"#)
        }
        Some("search") => {
            let q = args.get(1).map(String::as_str).unwrap_or("");
            format!(
                r#"{{"op":"search","query":{},"limit":20}}"#,
                serde_json::to_string(q).unwrap()
            )
        }
        Some("paste") => match args.get(1).and_then(|s| s.parse::<i64>().ok()) {
            Some(id) => format!(r#"{{"op":"paste","id":{id},"plain":false}}"#),
            None => die("usage: clipctl paste <id>"),
        },
        Some("copy") => match args.get(1).and_then(|s| s.parse::<i64>().ok()) {
            Some(id) => format!(r#"{{"op":"copy","id":{id}}}"#),
            None => die("usage: clipctl copy <id>"),
        },
        Some("delete") => match args.get(1).and_then(|s| s.parse::<i64>().ok()) {
            Some(id) => format!(r#"{{"op":"delete","id":{id}}}"#),
            None => die("usage: clipctl delete <id>"),
        },
        Some("ping") => json(r#"{"op":"ping"}"#),
        Some("watch") => {
            // Streams events until killed — the quickest way to see whether the
            // hotkey grab, capture and broadcast paths are all live.
            watch();
            return;
        }
        Some("capture-wayland") => {
            capture_wayland();
            return;
        }
        Some("help" | "-h" | "--help") => {
            println!("{USAGE}");
            return;
        }
        Some(other) => die(&format!("unknown command {other:?}\n\n{USAGE}")),
    };

    let path = clipd_ipc::socket_path();
    let mut stream = match UnixStream::connect(&path) {
        Ok(s) => s,
        Err(e) => die(&format!(
            "cannot reach clipd at {}: {e}\nIs the daemon running?",
            path.display()
        )),
    };

    if let Err(e) = writeln!(stream, "{request}").and_then(|_| stream.flush()) {
        die(&format!("write failed: {e}"));
    }

    // One request, one response line.
    let mut line = String::new();
    if BufReader::new(&stream).read_line(&mut line).is_ok() && !line.trim().is_empty() {
        print!("{}", pretty(&line));
    }
}

/// Render `Items` responses as one line per item; pass anything else through.
fn pretty(line: &str) -> String {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
        return line.to_string();
    };
    let Some(items) = v.get("items").and_then(|i| i.as_array()) else {
        return format!("{line}");
    };
    let mut out = String::new();
    for item in items {
        let id = item.get("id").and_then(|v| v.as_i64()).unwrap_or(0);
        let kind = item.get("kind").and_then(|v| v.as_str()).unwrap_or("?");
        let pinned = item.get("pinned").and_then(|v| v.as_bool()).unwrap_or(false);
        let sensitive = item.get("sensitive").and_then(|v| v.as_bool()).unwrap_or(false);
        let preview = if sensitive {
            "<hidden>"
        } else {
            item.get("preview").and_then(|v| v.as_str()).unwrap_or("")
        };
        out.push_str(&format!(
            "{:>5}  {}{:<6} {}\n",
            id,
            if pinned { "*" } else { " " },
            kind,
            preview
        ));
    }
    out
}

/// Subscribe and print every pushed event, one per line, until interrupted.
fn watch() {
    let path = clipd_ipc::socket_path();
    let mut stream = match UnixStream::connect(&path) {
        Ok(s) => s,
        Err(e) => die(&format!("cannot reach clipd at {}: {e}", path.display())),
    };
    if writeln!(stream, r#"{{"op":"subscribe"}}"#).and_then(|_| stream.flush()).is_err() {
        die("subscribe failed");
    }
    for line in BufReader::new(stream).lines() {
        match line {
            Ok(l) if !l.trim().is_empty() => {
                println!("{l}");
                let _ = std::io::stdout().flush();
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
}

fn json(s: &str) -> String {
    s.to_string()
}

fn die(msg: &str) -> ! {
    eprintln!("clipctl: {msg}");
    std::process::exit(1)
}

const USAGE: &str = "\
usage: clipctl <command>

  toggle              show or hide the popup (default)
  list [N]            print the N most recent items
  search <query>      search history
  paste <id>          put an item on the clipboard and paste it
  copy <id>           put an item on the clipboard without pasting
  delete <id>         remove an item
  watch               stream events (captures, hotkey, deletions)
  capture-wayland     internal wl-paste watcher helper
  ping                check the daemon is alive";

fn capture_wayland() {
    let output = match std::process::Command::new("wl-paste")
        .args(["--list-types"])
        .output()
    {
        Ok(output) if output.status.success() => output,
        _ => return,
    };

    let types = String::from_utf8_lossy(&output.stdout);
    let wanted = [
        "image/png",
        "image/jpeg",
        "image/webp",
        "image/gif",
        "image/bmp",
        "text/html",
        "text/uri-list",
        "text/plain;charset=utf-8",
        "text/plain",
    ];
    let mut flavors = Vec::new();
    for mime in wanted {
        if !types.lines().any(|line| line.trim() == mime) {
            continue;
        }
        let Ok(data) = std::process::Command::new("wl-paste")
            .args(["--no-newline", "--type", mime])
            .output()
        else {
            continue;
        };
        if !data.status.success() || data.stdout.is_empty() {
            continue;
        }
        flavors.push(serde_json::json!({
            "mime": mime,
            "data": base64::engine::general_purpose::STANDARD.encode(data.stdout),
        }));
    }

    if flavors.is_empty() {
        return;
    }
    let hinted_secret = types.lines().any(|line| {
        line.trim() == "x-kde-passwordManagerHint"
    });
    let request = serde_json::json!({
        "op": "capture",
        "flavors": flavors,
        "hinted_secret": hinted_secret,
    });
    let path = clipd_ipc::socket_path();
    let Ok(mut stream) = UnixStream::connect(path) else { return };
    let _ = writeln!(stream, "{request}").and_then(|_| stream.flush());
}
