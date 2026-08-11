import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { Item } from "../../../crates/clipd-ipc/bindings/Item";
import type { Event as ClipEvent } from "../../../crates/clipd-ipc/bindings/Event";

export type { Item, ClipEvent };

export const api = {
  recent: (limit = 300) => invoke<Item[]>("recent", { limit }),
  search: (query: string, limit = 300) => invoke<Item[]>("search", { query, limit }),
  paste: (id: number, plain = false) => invoke<void>("paste", { id, plain }),
  copy: (id: number) => invoke<void>("copy_item", { id }),
  remove: (id: number) => invoke<void>("delete", { id }),
  setPinned: (id: number, pinned: boolean) => invoke<void>("set_pinned", { id, pinned }),
  setFavorite: (id: number, favorite: boolean) =>
    invoke<void>("set_favorite", { id, favorite }),
  clearAll: () => invoke<void>("clear_all"),
  hide: () => invoke<void>("hide_popup"),
  /**
   * Always true on X11. On Wayland, reflects whether the RemoteDesktop portal
   * session has actually been granted keyboard access — check fresh before
   * each paste, don't cache: it can flip true mid-session once the user
   * answers the one-time permission dialog.
   */
  canAutopaste: () => invoke<boolean>("can_autopaste"),
};

export const onDaemonEvent = (fn: (e: ClipEvent) => void) =>
  listen<ClipEvent>("clipd://event", (e) => fn(e.payload));

/// Fired when the window is shown, so the list can reset without remounting.
export const onOpened = (fn: () => void) => listen("clipd://opened", () => fn());
