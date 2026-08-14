//! SPDX-License-Identifier: GPL-3.0-or-later
//! Copyright (C) 2026 Ganesh Bastapure
//!
//! Unix-socket server: request/response plus a push stream of events.
//!
//! Reads that only touch SQLite are answered on the connection thread. Anything
//! needing the X server is forwarded to the main loop, because the platform
//! backend must have exactly one driver.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use clipd_core::{Captured, Store};
use clipd_ipc::{encode, Event, Request, Response};

/// Work that must run on the thread owning the X11 connection.
pub enum Action {
    /// Put an item on the clipboard, optionally injecting a paste.
    Offer { id: i64, paste: bool, plain: bool },
    /// Put transient flavors on the clipboard, optionally injecting paste.
    OfferFlavors { flavors: Vec<(String, Vec<u8>)>, paste: bool },
    /// A capture delivered by the wl-paste watcher.
    Capture(Box<Captured>),
    /// Snapshot the focused window before the popup takes focus.
    RememberFocus,
    /// Hand keyboard focus to the popup, which cannot do it for itself.
    FocusPopup,
}

pub enum Msg {
    Signal(clipd_platform::Signal),
    Action(Action),
}

/// Tracks subscribed connections and fans events out to them.
pub struct Hub {
    subs: Mutex<Vec<UnixStream>>,
}

impl Hub {
    pub fn new() -> Self {
        Self { subs: Mutex::new(Vec::new()) }
    }

    fn subscribe(&self, stream: UnixStream) {
        self.subs.lock().unwrap().push(stream);
    }

    /// Push to every subscriber, dropping any that has gone away.
    ///
    /// This is the only place subscribers are reaped — a failed write means the
    /// peer is gone, so no separate disconnect bookkeeping is needed.
    pub fn broadcast(&self, event: &Event) {
        let Ok(frame) = encode(event) else { return };
        let mut subs = self.subs.lock().unwrap();
        subs.retain_mut(|s| s.write_all(&frame).and_then(|_| s.flush()).is_ok());
    }
}

impl Default for Hub {
    fn default() -> Self {
        Self::new()
    }
}

/// Bind the socket and start accepting. Returns immediately.
pub fn listen(hub: Arc<Hub>, store: Arc<Mutex<Store>>, tx: Sender<Msg>) -> Result<()> {
    let path = clipd_ipc::socket_path();

    // Unix socket paths are capped at ~108 bytes by the kernel, and the raw
    // error ("path must be shorter than SUN_LEN") gives no clue which path is
    // at fault. Say it plainly instead.
    if path.as_os_str().len() >= 100 {
        anyhow::bail!(
            "socket path is too long ({} bytes, limit ~100): {}\n\
             Set XDG_RUNTIME_DIR to a shorter directory.",
            path.as_os_str().len(),
            path.display()
        );
    }

    // A socket file left by a crashed daemon would block binding. Only remove
    // it if nothing is actually listening, so we never displace a live daemon.
    if path.exists() {
        if UnixStream::connect(&path).is_ok() {
            anyhow::bail!("another clipd is already listening on {}", path.display());
        }
        std::fs::remove_file(&path)?;
    }

    let listener = UnixListener::bind(&path)?;
    // History is sensitive by nature; keep it to the owning user.
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;

    std::thread::Builder::new().name("clipd-accept".into()).spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let hub = Arc::clone(&hub);
            let store = Arc::clone(&store);
            let tx = tx.clone();
            std::thread::Builder::new()
                .name("clipd-conn".into())
                .spawn(move || {
                    if let Err(e) = serve_conn(stream, hub, store, tx) {
                        eprintln!("clipd: connection ended: {e}");
                    }
                })
                .ok();
        }
    })?;

    Ok(())
}

fn serve_conn(stream: UnixStream, hub: Arc<Hub>, store: Arc<Mutex<Store>>, tx: Sender<Msg>) -> Result<()> {
    let mut out = stream.try_clone()?;
    let reader = BufReader::new(stream);

    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let request: Request = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                out.write_all(&encode(&Response::Error { message: e.to_string() })?)?;
                continue;
            }
        };

        let response = handle(request, &hub, &store, &tx, &out)?;
        if let Some(response) = response {
            out.write_all(&encode(&response)?)?;
            out.flush()?;
        }
    }
    Ok(())
}

fn handle(
    request: Request,
    hub: &Arc<Hub>,
    store: &Arc<Mutex<Store>>,
    tx: &Sender<Msg>,
    out: &UnixStream,
) -> Result<Option<Response>> {
    let response = match request {
        Request::Ping => Response::Pong,

        Request::Recent { limit } => {
            let items = store.lock().unwrap().recent(limit.min(5000))?;
            Response::Items { items }
        }

        Request::Search { query, limit } => {
            let items = store.lock().unwrap().search(&query, limit.min(5000))?;
            Response::Items { items }
        }

        Request::Paste { id, plain } => {
            tx.send(Msg::Action(Action::Offer { id, paste: true, plain })).ok();
            Response::Ok
        }

        Request::Copy { id } => {
            tx.send(Msg::Action(Action::Offer { id, paste: false, plain: false })).ok();
            Response::Ok
        }

        Request::Capture { flavors, hinted_secret } => {
            use base64::Engine as _;
            let decoded = flavors
                .into_iter()
                .filter_map(|flavor| {
                    base64::engine::general_purpose::STANDARD
                        .decode(flavor.data)
                        .ok()
                        .map(|data| (flavor.mime, data))
                })
                .collect();
            tx.send(Msg::Action(Action::Capture(Box::new(Captured {
                flavors: decoded,
                source_app: None,
                hinted_secret,
            }))))
            .ok();
            Response::Ok
        }

        Request::Thumbnail { id } => {
            use base64::Engine as _;
            let data = store.lock().unwrap().thumbnail(id)?;
            let encoded = data.map(|bytes| {
                format!(
                    "data:image/png;base64,{}",
                    base64::engine::general_purpose::STANDARD.encode(bytes)
                )
            });
            Response::Thumbnail { data: encoded.unwrap_or_default() }
        }

        Request::ResizeCopy { id, width, height, keep_aspect_ratio } => {
            let item = {
                let mut store = store.lock().unwrap();
                let Some(captured) = store.resized_capture(id, width, height, keep_aspect_ratio)? else {
                    return Ok(Some(Response::Error { message: "selected item is not an image".into() }));
                };
                let item = store.insert(captured, false)?;
                if let Some(item) = &item {
                    hub.broadcast(&Event::Added { item: item.clone() });
                }
                item
            };
            if let Some(item) = item {
                tx.send(Msg::Action(Action::Offer { id: item.id, paste: false, plain: false })).ok();
            }
            Response::Ok
        }

        Request::SetText { text, paste } => {
            let captured = Captured {
                flavors: vec![("UTF8_STRING".into(), text.into_bytes())],
                source_app: None,
                hinted_secret: false,
            };
            store.lock().unwrap().mark_head(&captured);
            tx.send(Msg::Action(Action::OfferFlavors {
                flavors: captured.flavors,
                paste,
            }))
            .ok();
            Response::Ok
        }

        Request::Delete { id } => {
            store.lock().unwrap().delete(id)?;
            hub.broadcast(&Event::Removed { id });
            Response::Ok
        }

        Request::Pin { id, pinned } => {
            let item = {
                let store = store.lock().unwrap();
                store.set_pinned(id, pinned)?;
                store.get(id)?
            };
            if let Some(item) = item {
                hub.broadcast(&Event::Updated { item });
            }
            Response::Ok
        }

        Request::Favorite { id, favorite } => {
            let item = {
                let store = store.lock().unwrap();
                store.set_favorite(id, favorite)?;
                store.get(id)?
            };
            if let Some(item) = item {
                hub.broadcast(&Event::Updated { item });
            }
            Response::Ok
        }

        Request::ClearAll => {
            store.lock().unwrap().clear_all()?;
            hub.broadcast(&Event::Cleared);
            Response::Ok
        }

        Request::TogglePopup => {
            // Snapshot focus before the UI takes it, then tell the UI to show.
            tx.send(Msg::Action(Action::RememberFocus)).ok();
            hub.broadcast(&Event::Toggle);
            Response::Ok
        }

        Request::FocusPopup => {
            tx.send(Msg::Action(Action::FocusPopup)).ok();
            Response::Ok
        }

        Request::Subscribe => {
            hub.subscribe(out.try_clone()?);
            Response::Ok
        }
    };
    Ok(Some(response))
}
