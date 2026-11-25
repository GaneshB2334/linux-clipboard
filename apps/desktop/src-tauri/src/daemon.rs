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
use std::time::Duration;

use clipd_ipc::{Item, Request, Response};

/// Bound short-lived request sockets so a wedged daemon can never hang the UI.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

pub fn request(req: &Request) -> Result<Response, String> {
    let path = clipd_ipc::socket_path();
    let mut stream = UnixStream::connect(&path)
        .map_err(|e| format!("clipd is not running ({}): {e}", path.display()))?;
    let _ = stream.set_read_timeout(Some(REQUEST_TIMEOUT));
    let _ = stream.set_write_timeout(Some(REQUEST_TIMEOUT));

    let frame = clipd_ipc::encode(req).map_err(|e| e.to_string())?;
    stream.write_all(&frame).map_err(|e| e.to_string())?;
    stream.flush().map_err(|e| e.to_string())?;

    let mut line = String::new();
    BufReader::new(&stream)
        .read_line(&mut line)
        .map_err(|e| e.to_string())?;
    serde_json::from_str(&line).map_err(|e| e.to_string())
}

pub fn ok(req: &Request) -> Result<(), String> {
    match request(req)? {
        Response::Ok => Ok(()),
        Response::Error { message } => Err(message),
        other => Err(format!("unexpected response: {other:?}")),
    }
}

pub fn items(req: &Request) -> Result<Vec<Item>, String> {
    match request(req)? {
        Response::Items { items } => Ok(items),
        Response::Error { message } => Err(message),
        other => Err(format!("unexpected response: {other:?}")),
    }
}

/// Open a subscription, invoke `on_connected` once the daemon acknowledges it,
/// and call `on_event` for every pushed event.
///
/// Blocks; run it on its own thread. Returns when the daemon goes away so the
/// caller can retry — the daemon may legitimately restart under us.
pub fn subscribe(
    mut on_connected: impl FnMut(),
    mut on_event: impl FnMut(clipd_ipc::Event),
) -> Result<(), String> {
    let path = clipd_ipc::socket_path();
    let mut stream = UnixStream::connect(&path).map_err(|e| e.to_string())?;
    let _ = stream.set_write_timeout(Some(REQUEST_TIMEOUT));
    let frame = clipd_ipc::encode(&Request::Subscribe).map_err(|e| e.to_string())?;
    stream.write_all(&frame).map_err(|e| e.to_string())?;
    stream.flush().map_err(|e| e.to_string())?;

    let mut acknowledged = false;
    for line in BufReader::new(stream).lines() {
        let line = line.map_err(|e| e.to_string())?;
        if line.trim().is_empty() {
            continue;
        }
        if !acknowledged {
            acknowledged = true;
            on_connected();
            if serde_json::from_str::<Response>(&line).is_ok() {
                continue;
            }
        }
        if let Ok(event) = serde_json::from_str::<clipd_ipc::Event>(&line) {
            on_event(event);
        }
    }
    Ok(())
}
