# Phase 0 — de-risk spike results

Measured on 2026-08-05: Ubuntu 24.04.4, GNOME Shell 46.0, X11, AMD/amdgpu, Rust 1.97.1.

**Gate verdict: PASSED.** Both load-bearing assumptions hold on X11 — clipboard capture is
event-driven and cheap, and synthetic paste with focus restore reaches a real application.
Two design corrections came out of it, both now folded into the plan.

## Measured

| Metric | Target | Measured |
|---|---|---|
| Capture daemon RSS | <20 MB | **2.1 MB** |
| Idle CPU (10s, no clipboard activity) | <1% | **0.000%** (0 scheduler ticks) |
| Clipboard event → data in hand | — | **1.1–3.7 ms** (TARGETS ~1.0 ms, payload ~0.6 ms) |
| Popup window map + focus | — | **23 ms** |
| Focus restore to prior window | — | **30 ms** |
| XTEST Ctrl+V injection | — | **0.06 ms** |
| Popup open → paste delivered | <100 ms overhead | **~54 ms** (454 ms total less the 400 ms simulated read time) |

Paste was verified against `gnome-text-editor`: focus restored to the correct window, and the
editor subsequently requested `UTF8_STRING` from us — the app actually consumed the paste, which
is the only real proof it worked.

## Finding 1 — GNOME re-offers every copy, with custom targets stripped

Every copy produced **two** `SetSelectionOwner` events. The second carries a different target
list (`text/plain;charset=utf-8 UTF8_STRING TARGETS TIMESTAMP` vs. the app's own), because
GNOME Shell takes CLIPBOARD ownership when the owning app **exits**, to keep the content alive.

Consequences:

- **The MIME-marker loop guard alone is not sufficient.** GNOME's re-offer does not carry our
  `application/x-clipd-serial` target, so our own paste comes back looking like a foreign copy.
  Observed directly: `skipped: our own paste (loop guard held)` immediately followed by the same
  content being ingested.
- **A time window cannot fix it.** The hand-off fires on app exit — measured at 2000–2010 ms
  after the copy in the test, but unbounded in general.
- **Fix (implemented and verified):** if incoming content is byte-identical to the current head
  item, it is not a new copy — drop it entirely. Time-independent. Verified 4 copies → 4 entries
  and 4 ignored re-offers, while re-copying an item that was *not* at the head was still
  correctly accepted and moved to the top.

Without this, every copy is counted twice and any "most used" ranking is meaningless.

## Finding 2 — `GetInputFocus` returns a child, not the toplevel

Focus restore targeted `0x2600004` but `GetInputFocus` reported `0x2600005`. Comparing the two
directly would report a spurious mismatch. The check must walk up via `QueryTree` until it hits
the target or the root.

This matters because of Finding 3.

## Finding 3 — never inject Ctrl+V without confirming focus first

A synthetic Ctrl+V goes to whatever holds focus. If focus restore fails, it pastes into an
unrelated document. The spike now verifies focus landed on the intended window and **aborts the
injection** otherwise, leaving the content on the clipboard for a manual paste. This is correct
product behaviour, not just test hygiene, and should ship in the daemon.

## Finding 4 — clipboard images from short-lived processes are not retrievable

`gnome-screenshot -c` advertised `image/png` in TARGETS but refused to serve the payload
(`owner refused the payload`). `gnome-screenshot` exits immediately, and GNOME's persistence
hand-off preserves the target list without being able to serve large image data.

This **validates the planned screenshot design**: capture the image ourselves and insert it
straight into history, rather than shelling out to a screenshot tool and reading it back off the
clipboard. The naive approach does not work on GNOME.

## Finding 5 — content identity must exclude the flavor list (found in Phase 1)

The first end-to-end daemon run produced **two rows per copy**. The head-hash
suppression from Finding 1 did not fire, because the hash covered every offered
MIME type — and GNOME's re-offer carries a *different* list than the source app:

```
source app : TARGETS TIMESTAMP UTF8_STRING TEXT STRING
GNOME       : text/plain;charset=utf-8 UTF8_STRING TARGETS TIMESTAMP
```

Same bytes, different flavor set, different hash, second row.

Identity is now computed from content alone — image pixels if present, otherwise
the text — never from the set of types offered. Covered by
`reoffer_with_different_flavor_list_is_still_the_same_copy`, which encodes the
two lists above verbatim.

The Phase 0 spike missed this because it hashed only the chosen payload; the
daemon stores all flavors, so the bug only appeared once the real store existed.

## Finding 6 — XGrabKey does not work under GNOME (found in Phase 1)

`GrabKey` for `Super+V` returned **success** on all four lock-modifier variants,
and then delivered nothing — zero `KeyPress` events, including from a minimal
spike using `wait_for_event` directly, and including for `Ctrl+Alt+V`.

Cause: GNOME Shell registers its shortcuts as **XI2 passive grabs**
(`XIGrabKeycode`). XI2 and core grabs are tracked separately, so a core
`GrabKey` registers without `BadAccess` and is then shadowed — Mutter consumes
the key first.

Consequence: the hotkey must come from GNOME's own mechanism, a
`custom-keybinding` in gsettings that spawns `clipctl toggle`. This is a better
answer anyway — it is the same code path Wayland needs, where `XGrabKey` does
not exist at all. `scripts/install-hotkey.sh` registers it, refuses to stomp on
a binding another app owns, and has `--uninstall`.

The daemon now skips the core grab entirely when `XDG_CURRENT_DESKTOP` contains
GNOME, rather than reporting a success that will never fire. The `XGrabKey` path
is retained for plain X11 window managers (i3, XFCE, Openbox) that have no
equivalent mechanism.

*Caveat on the diagnosis:* a `Ctrl+Alt+V` control test was contaminated — that
combination is bound by another application on this machine. The conclusion
rests on `Super+V` plus the minimal-spike result.

## Phase 1 — measured (2026-08-10)

| Metric | Target | Measured |
|---|---|---|
| Daemon RSS / PSS, idle | <20 MB | **5.1 MB / 3.0 MB** |
| Daemon idle CPU | <1% | **0.000%** (0 ticks over 10 s) |
| Warm popup open | <100 ms | **9–11 ms** (24 ms on the first open) |
| Warm UI process tree, PSS | — | **~152 MB** |
| Clipboard event → stored | — | 1–4 ms |

Beware summing RSS across the webview process tree: it double-counts shared
pages and reports ~340 MB for what PSS shows as ~152 MB.

## Still unverified

- **INCR transfers.** The receive side is implemented (chunked `PropertyNotify` loop, 10 s
  budget, cap-aware) but nothing in the test set actually sends INCR — our own writer fits
  payloads in a single request under BIG-REQUESTS. Needs a manual check: copy a large image from
  Firefox or GIMP and confirm the chunked path completes. Phase 1 test item.
- **Everything Wayland.** Untested by design; this machine is X11 and GNOME 46 lacks
  `data-control`. The portal path (RemoteDesktop + Clipboard) is a Phase 4 spike of its own.
- **Warm-window show latency.** Needs the Tauri shell, which needs `libwebkit2gtk-4.1-dev`
  (not installed — requires sudo).

## Running the spikes

```bash
cargo build --release

# terminal 1: watch the clipboard
./target/release/x11-capture

# terminal 2: write to the clipboard as a foreign app would
./target/release/x11-paste --set-only --no-marker "some text"

# full paste sequence — focus a target app during the countdown
./target/release/x11-paste --arm=5 "text to paste"
```
