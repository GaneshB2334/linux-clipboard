// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Ganesh Bastapure

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { Item } from "../../../crates/clipd-ipc/bindings/Item";
import type { Event as ClipEvent } from "../../../crates/clipd-ipc/bindings/Event";

export type { Item, ClipEvent };

const THUMBNAIL_CACHE_LIMIT = 64;
const thumbnailCache = new Map<number, string>();
const thumbnailInFlight = new Map<number, Promise<string>>();

/** Synchronous cache read so returning virtualized rows render with zero flicker. */
export function getCachedThumbnail(id: number): string | undefined {
  const hit = thumbnailCache.get(id);
  if (hit !== undefined) {
    // Refresh LRU recency.
    thumbnailCache.delete(id);
    thumbnailCache.set(id, hit);
  }
  return hit;
}

export function evictThumbnail(id: number): void {
  thumbnailCache.delete(id);
  thumbnailInFlight.delete(id);
}

export function clearThumbnailCache(): void {
  thumbnailCache.clear();
  thumbnailInFlight.clear();
}

function fetchThumbnail(id: number): Promise<string> {
  const cached = getCachedThumbnail(id);
  if (cached !== undefined) return Promise.resolve(cached);

  const pending = thumbnailInFlight.get(id);
  if (pending) return pending;

  const request = invoke<string>("thumbnail", { id })
    .then((data) => {
      if (data) {
        if (thumbnailCache.size >= THUMBNAIL_CACHE_LIMIT) {
          const oldest = thumbnailCache.keys().next().value;
          if (oldest !== undefined) thumbnailCache.delete(oldest);
        }
        thumbnailCache.set(id, data);
      }
      return data;
    })
    .finally(() => {
      thumbnailInFlight.delete(id);
    });

  thumbnailInFlight.set(id, request);
  return request;
}

export const api = {
  recent: (limit = 300) => invoke<Item[]>("recent", { limit }),
  search: (query: string, limit = 300) => invoke<Item[]>("search", { query, limit }),
  thumbnail: fetchThumbnail,
  /** Returns true if auto-paste was triggered (popup hidden), false if only copied. */
  paste: (id: number, plain = false) => invoke<boolean>("paste", { id, plain }),
  /** Returns true if auto-paste was triggered (popup hidden), false if only copied. */
  pasteText: (text: string) => invoke<boolean>("paste_text", { text }),
  resizeCopy: (id: number, width: number, height?: number) =>
    invoke<void>("resize_copy", { id, width, height }),
  copy: (id: number) => invoke<void>("copy_item", { id }),
  remove: (id: number) => invoke<void>("delete", { id }),
  setPinned: (id: number, pinned: boolean) => invoke<void>("set_pinned", { id, pinned }),
  setFavorite: (id: number, favorite: boolean) =>
    invoke<void>("set_favorite", { id, favorite }),
  clearAll: () => invoke<void>("clear_all"),
  hide: () => invoke<void>("hide_popup"),
  /**
   * Always true on X11. On Wayland, reflects whether uinput or the GNOME
   * extension is available right now — checked fresh per call.
   */
  canAutopaste: () => invoke<boolean>("can_autopaste"),
  /**
   * Fired the instant a press lands on the header's drag area, before Tauri's
   * own drag-region script asks the window manager to start moving the
   * window. That handshake causes a brief, spurious focus-loss event — see
   * the Rust side's `drag_hint` command for why this exists.
   */
  dragHint: () => invoke<void>("drag_hint"),
};

export const onDaemonEvent = (fn: (e: ClipEvent) => void) =>
  listen<ClipEvent>("clipd://event", (e) => fn(e.payload));

/** Fired when the window is shown, so the list can reset without remounting. */
export const onOpened = (fn: () => void) => listen("clipd://opened", () => fn());

/** Fired whenever the daemon subscription connects or reconnects. */
export const onSync = (fn: () => void) => listen("clipd://sync", () => fn());
