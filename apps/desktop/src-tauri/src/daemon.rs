//! SPDX-License-Identifier: GPL-3.0-or-later
//! Copyright (C) 2026 Ganesh Bastapure
//!
//! Client for the clipd socket.
//!
//! Requests open a short-lived connection: a Unix-socket round trip is tens of
//! microseconds, and a per-request connection avoids sharing a read buffer with
//! the long-lived subscription. The subscription is the important one — it is
//! what keeps the UI's list current so that opening the popup needs no query.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;

use clipd_ipc::{Item, Request, Response};

pub fn request(req: &Request) -> Result<Response, String> {
    let path = clipd_ipc::socket_path();
    let mut stream = UnixStream::connect(&path)
        .map_err(|e| format!("clipd is not running ({}): {e}", path.display()))?;

    let frame = clipd_ipc::encode(req).map_err(|e| e.to_string())?;
    stream.write_all(&frame).map_err(|e| e.to_string())?;
    stream.flush().map_err(|e| e.to_string())?;

    let mut line = String::new();
    BufReader::new(&stream)
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    serde_json::from_str(&line).map_err(|e| e.to_string())
}

pub fn items(req: &Request) -> Result<Vec<Item>, String> {
    match request(req)? {
        Response::Items { items } => Ok(items),
        Response::Error { message } => Err(message),
        other => Err(format!("unexpected response: {other:?}")),
    }
}

/// Open a subscription and call `on_event` for every pushed event.
///
/// Blocks; run it on its own thread. Returns when the daemon goes away so the
/// caller can retry — the daemon may legitimately restart under us.
pub fn subscribe(mut on_event: impl FnMut(clipd_ipc::Event)) -> Result<(), String> {
    let path = clipd_ipc::socket_path();
    let mut stream = UnixStream::connect(&path).map_err(|e| e.to_string())?;
    let frame = clipd_ipc::encode(&Request::Subscribe).map_err(|e| e.to_string())?;
    stream.write_all(&frame).map_err(|e| e.to_string())?;
    stream.flush().map_err(|e| e.to_string())?;

    for line in BufReader::new(stream).lines() {
        let line = line.map_err(|e| e.to_string())?;
        if line.trim().is_empty() {
            continue;
        }
        // The first line is the Ok acknowledging the subscription.
        if let Ok(event) = serde_json::from_str::<clipd_ipc::Event>(&line) {
            on_event(event);
        }
    }
    Ok(())
}
