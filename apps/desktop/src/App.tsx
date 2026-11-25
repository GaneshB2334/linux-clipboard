// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Ganesh Bastapure

import {
  Suspense,
  cloneElement,
  lazy,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactElement,
  type ReactNode,
} from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import {
  ArrowUpDown,
  Clipboard,
  ClipboardX,
  CornerDownLeft,
  GripHorizontal,
  Layers3,
  Pin,
  Search,
  SearchX,
  ShieldCheck,
  SmilePlus,
  TextCursorInput,
  Trash2,
  X,
} from "lucide-react";
import {
  api,
  clearThumbnailCache,
  evictThumbnail,
  onDaemonEvent,
  onOpened,
  onSync,
  type Item,
} from "./api";
import { filterItems, mergeResults, type Match } from "./filter";
import { Row } from "./Row";
import { Preview } from "./Preview";
import type { EmojiTone } from "./emoji";

/** Lazy-loaded so the ~800 KB emojibase dataset stays off the initial popup startup path. */
const EmojiPicker = lazy(() => import("./EmojiPicker"));
const EMOJI_COLS = 10;

type EmojiModule = typeof import("./emoji");

/** How many items the hot list holds. Tier-1 filtering stays instant well past this. */
const HOT_SIZE = 300;
const ROW_HEIGHT = 56;

/** Keep the hot list in the exact same order as SQLite: pinned first, then most recently used. */
function upsertItem(prev: Item[], item: Item): Item[] {
  const next = [item, ...prev.filter((i) => i.id !== item.id)];
  next.sort((a, b) => {
    if (a.pinned !== b.pinned) return a.pinned ? -1 : 1;
    const diff = Number(b.last_used_at) - Number(a.last_used_at);
    return diff !== 0 ? diff : b.id - a.id;
  });
  return next.slice(0, HOT_SIZE);
}

export default function App() {
  const [items, setItems] = useState<Item[]>([]);
  const [query, setQuery] = useState("");
  const [cold, setCold] = useState<Item[]>([]);
  const [selected, setSelected] = useState(0);
  const [emojiSelected, setEmojiSelected] = useState(0);
  const [emojiGroup, setEmojiGroup] = useState<number | null>(null);
  const [emojiTone, setEmojiTone] = useState<EmojiTone>(0);
  const [emojiMod, setEmojiMod] = useState<EmojiModule | null>(null);
  const [now, setNow] = useState(() => Date.now());
  const [recentEmojis, setRecentEmojis] = useState<string[]>(() => {
    try {
      const stored = localStorage.getItem("clipd.recent-emojis");
      return stored ? JSON.parse(stored) : [];
    } catch {
      return [];
    }
  });
  const [activeTab, setActiveTab] = useState<"clipboard" | "emoji">("clipboard");
  const [toast, setToast] = useState<string | null>(null);

  const inputRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  const headerRef = useRef<HTMLElement>(null);
  const toastTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const pastingRef = useRef(false);

  const clearToastTimer = useCallback(() => {
    if (toastTimerRef.current !== null) {
      clearTimeout(toastTimerRef.current);
      toastTimerRef.current = null;
    }
  }, []);

  const showToast = useCallback(
    (msg: string, durationMs = 1800, hideAfter = false) => {
      clearToastTimer();
      setToast(msg);
      toastTimerRef.current = setTimeout(() => {
        toastTimerRef.current = null;
        setToast(null);
        if (hideAfter) void api.hide();
      }, durationMs);
    },
    [clearToastTimer],
  );

  useEffect(() => clearToastTimer, [clearToastTimer]);

  // ---- data ------------------------------------------------------------

  const reloadRecent = useCallback(() => {
    api.recent(HOT_SIZE).then(setItems).catch(() => setItems([]));
  }, []);

  useEffect(() => {
    reloadRecent();
  }, [reloadRecent]);

  // Re-sync after initial connect or daemon restart so the list never goes stale.
  useEffect(() => {
    const un = onSync(reloadRecent);
    return () => {
      un.then((f) => f());
    };
  }, [reloadRecent]);

  // Keep the hot list current from the daemon's push stream. This is what lets
  // the popup open without querying anything.
  useEffect(() => {
    const un = onDaemonEvent((event) => {
      if (event.event === "notice") {
        showToast(event.message, 3200, true);
        return;
      }
      if (event.event === "removed") {
        evictThumbnail(event.id);
      } else if (event.event === "cleared") {
        clearThumbnailCache();
      }
      setItems((prev) => {
        switch (event.event) {
          case "added":
          case "updated":
            return upsertItem(prev, event.item);
          case "removed":
            return prev.filter((i) => i.id !== event.id);
          case "cleared":
            return prev.filter((i) => i.pinned);
          default:
            return prev;
        }
      });
    });
    return () => {
      un.then((f) => f());
    };
  }, [showToast]);

  // Reset to a predictable state every time the popup opens. Deliberately not
  // preserving the previous query: reopening should always mean "top of list".
  useEffect(() => {
    const un = onOpened(() => {
      clearToastTimer();
      pastingRef.current = false;
      setToast(null);
      setQuery("");
      setCold([]);
      setSelected(0);
      setEmojiSelected(0);
      setActiveTab("clipboard");
      setNow(Date.now());
      listRef.current?.scrollTo({ top: 0 });
      requestAnimationFrame(() => inputRef.current?.focus());
    });
    return () => {
      un.then((f) => f());
    };
  }, [clearToastTimer]);

  // Tier 2: the cold tail. Debounced, and merged in when it arrives — the user
  // is already looking at tier-1 results by then.
  useEffect(() => {
    if (activeTab !== "clipboard" || !query.trim()) {
      setCold([]);
      return;
    }
    let cancelled = false;
    const t = setTimeout(() => {
      api
        .search(query, HOT_SIZE)
        .then((res) => {
          if (!cancelled) setCold(res);
        })
        .catch(() => {
          if (!cancelled) setCold([]);
        });
    }, 60);
    return () => {
      cancelled = true;
      clearTimeout(t);
    };
  }, [activeTab, query]);

  useEffect(() => {
    if (activeTab !== "emoji" || emojiMod) return;
    let alive = true;
    import("./emoji")
      .then((mod) => {
        if (alive) setEmojiMod(mod);
      })
      .catch(() => undefined);
    return () => {
      alive = false;
    };
  }, [activeTab, emojiMod]);

  const matches: Match[] = useMemo(() => {
    const hot = filterItems(query, items);
    return query.trim() ? mergeResults(hot, cold, query) : hot;
  }, [query, items, cold]);

  const emojiMatches = useMemo(
    () => (emojiMod ? emojiMod.filterEmojis(query, emojiGroup, recentEmojis) : []),
    [emojiMod, query, emojiGroup, recentEmojis],
  );

  // Keep the selection in range as results change under it.
  useEffect(() => {
    setSelected((s) => Math.min(s, Math.max(0, matches.length - 1)));
  }, [matches.length]);

  useEffect(() => {
    setEmojiSelected((s) => Math.min(s, Math.max(0, emojiMatches.length - 1)));
  }, [emojiMatches.length]);

  // ---- virtualised list ------------------------------------------------

  const virtualizer = useVirtualizer({
    count: matches.length,
    getScrollElement: () => listRef.current,
    estimateSize: () => ROW_HEIGHT,
    overscan: 8,
  });

  useEffect(() => {
    if (activeTab === "clipboard" && matches.length) virtualizer.scrollToIndex(selected, { align: "auto" });
  }, [activeTab, selected, matches.length, virtualizer]);

  const flash = useCallback((msg: string) => showToast(msg, 1800, false), [showToast]);

  const changeTab = useCallback((tab: "clipboard" | "emoji") => {
    setActiveTab(tab);
    setQuery("");
    setCold([]);
    if (tab === "clipboard") setSelected(0);
    else setEmojiSelected(0);
    inputRef.current?.focus();
  }, []);

  // ---- actions ---------------------------------------------------------

  const act = useCallback(
    async (index: number, action: "paste" | "plain" | "copy" | "delete" | "pin") => {
      const item = matches[index]?.item;
      if (!item) return;
      switch (action) {
        case "paste":
        case "plain": {
          if (action === "plain" && item.kind === "image") {
            flash("Images cannot be pasted as plain text");
            return;
          }
          // Guard against double-clicks or repeated Enter firing two pastes.
          if (pastingRef.current) return;
          pastingRef.current = true;
          try {
            const autoPasted = await api.paste(item.id, action === "plain");
            if (!autoPasted) {
              showToast("Copied — press Ctrl+V to paste", 1100, true);
            }
          } catch (error) {
            flash(String(error));
          } finally {
            pastingRef.current = false;
          }
          break;
        }
        case "copy":
          try {
            await api.copy(item.id);
          } catch (error) {
            flash(String(error));
          }
          break;
        case "delete":
          try {
            await api.remove(item.id);
            flash("Deleted");
          } catch (error) {
            flash(String(error));
          }
          break;
        case "pin":
          try {
            await api.setPinned(item.id, !item.pinned);
          } catch (error) {
            flash(String(error));
          }
          break;
      }
    },
    [matches, flash, showToast],
  );

  const handleActivateRow = useCallback(
    (index: number) => {
      void act(index, "paste");
    },
    [act],
  );

  const pasteEmoji = useCallback(
    async (emoji: string) => {
      if (pastingRef.current) return;
      pastingRef.current = true;
      try {
        setRecentEmojis((previous) => {
          const next = [emoji, ...previous.filter((value) => value !== emoji)].slice(0, 24);
          try {
            localStorage.setItem("clipd.recent-emojis", JSON.stringify(next));
          } catch {
            // Storage quota / private mode must not block pasting.
          }
          return next;
        });
        const autoPasted = await api.pasteText(emoji);
        if (!autoPasted) {
          showToast("Copied — press Ctrl+V to paste", 1100, true);
        }
      } catch (error) {
        flash(String(error));
      } finally {
        pastingRef.current = false;
      }
    },
    [flash, showToast],
  );

  // ---- keyboard --------------------------------------------------------

  const onKeyDown = useCallback(
    (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        void api.hide();
        return;
      }

      // Let auxiliary inputs (e.g. Preview's custom width/height fields) own
      // Enter, Tab, Delete, Home, and arrows without triggering list actions.
      const target = e.target as HTMLElement | null;
      if (
        target &&
        target !== inputRef.current &&
        (target.tagName === "INPUT" || target.tagName === "TEXTAREA")
      ) {
        return;
      }

      // Alt+1..9 pastes the Nth clipboard item outright. Bare digits stay
      // available for typing a search, which is the more common action by far.
      if (activeTab === "clipboard" && e.altKey && /^[1-9]$/.test(e.key)) {
        e.preventDefault();
        void act(Number(e.key) - 1, "paste");
        return;
      }

      if (e.ctrlKey && e.key.toLowerCase() === "e") {
        e.preventDefault();
        changeTab("emoji");
        return;
      }

      if (e.key === "Tab") {
        e.preventDefault();
        changeTab(activeTab === "clipboard" ? "emoji" : "clipboard");
        return;
      }

      switch (e.key) {
        case "ArrowDown":
          e.preventDefault();
          if (activeTab === "emoji") {
            setEmojiSelected((s) => Math.min(s + EMOJI_COLS, Math.max(0, emojiMatches.length - 1)));
          } else {
            setSelected((s) => Math.min(s + 1, Math.max(0, matches.length - 1)));
          }
          break;
        case "ArrowUp":
          e.preventDefault();
          if (activeTab === "emoji") setEmojiSelected((s) => Math.max(s - EMOJI_COLS, 0));
          else setSelected((s) => Math.max(s - 1, 0));
          break;
        case "ArrowRight":
          if (activeTab === "emoji") {
            e.preventDefault();
            setEmojiSelected((s) => Math.min(s + 1, Math.max(0, emojiMatches.length - 1)));
          }
          break;
        case "ArrowLeft":
          if (activeTab === "emoji") {
            e.preventDefault();
            setEmojiSelected((s) => Math.max(s - 1, 0));
          }
          break;
        case "PageDown":
          e.preventDefault();
          if (activeTab === "emoji") {
            setEmojiSelected((s) => Math.min(s + EMOJI_COLS * 5, Math.max(0, emojiMatches.length - 1)));
          } else {
            setSelected((s) => Math.min(s + 8, Math.max(0, matches.length - 1)));
          }
          break;
        case "PageUp":
          e.preventDefault();
          if (activeTab === "emoji") setEmojiSelected((s) => Math.max(s - EMOJI_COLS * 5, 0));
          else setSelected((s) => Math.max(s - 8, 0));
          break;
        case "Home":
          if (!query) {
            e.preventDefault();
            if (activeTab === "emoji") setEmojiSelected(0);
            else setSelected(0);
          }
          break;
        case "Enter":
          e.preventDefault();
          if (activeTab === "emoji") {
            const entry = emojiMatches[emojiSelected];
            if (entry && emojiMod) void pasteEmoji(emojiMod.emojiForTone(entry, emojiTone));
          } else if (e.shiftKey) void act(selected, "plain");
          else if (e.ctrlKey) void act(selected, "copy");
          else void act(selected, "paste");
          break;
        case "Delete":
          if (activeTab === "clipboard") {
            // Preserve forward-delete when editing in the middle of a search query.
            const input = inputRef.current;
            const editingMiddle =
              input &&
              document.activeElement === input &&
              query.length > 0 &&
              (input.selectionStart ?? query.length) < query.length;
            if (!editingMiddle) {
              e.preventDefault();
              void act(selected, "delete");
            }
          }
          break;
        case "p":
        case "P":
          if (activeTab === "clipboard" && e.ctrlKey) {
            e.preventDefault();
            void act(selected, "pin");
          }
          break;
      }
    },
    [act, activeTab, changeTab, emojiMatches, emojiMod, emojiSelected, emojiTone, matches.length, pasteEmoji, selected, query],
  );

  // The search field owns focus permanently, so every keystroke filters
  // without the user ever reaching for Ctrl+F.
  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  // On window, not the panel: clicking anything non-focusable (empty list
  // space, the preview pane, a row) blurs the input and hands focus to
  // <body>, which sits *above* the panel in the tree — a listener on the
  // panel would never see events that originate there. Esc (and every other
  // shortcut) has to keep working no matter what has focus, so the listener
  // lives here instead of in onKeyDown JSX.
  useEffect(() => {
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [onKeyDown]);

  // Tauri's own `data-tauri-drag-region` script asks the window manager to
  // start moving the window on every qualifying mousedown, which the WM
  // acknowledges with a brief, real `Focused(false)` — indistinguishable
  // from an actual focus loss unless something records that a drag might be
  // starting first (see drag_hint in lib.rs). Capture phase, not bubble:
  // Tauri's script calls `stopImmediatePropagation()`, which would otherwise
  // block a bubble-phase listener on window from ever running.
  useEffect(() => {
    const onPointerDown = (e: MouseEvent) => {
      const header = headerRef.current;
      const target = e.target as Element | null;
      if (header && target && header.contains(target) && !target.closest("input, button")) {
        void api.dragHint();
      }
    };
    window.addEventListener("mousedown", onPointerDown, { capture: true });
    return () => window.removeEventListener("mousedown", onPointerDown, { capture: true });
  }, []);

  const handleEmojiGroup = useCallback((group: number | null) => {
    setEmojiGroup(group);
    setEmojiSelected(0);
  }, []);

  const current = matches[selected]?.item;

  return (
    // Transparent inset around the panel: this is what the rounded corners
    // and shadow actually show against, now that the window itself has no
    // background of its own (tauri.conf.json's `transparent: true`).
    <div className="h-screen w-full p-2.5">
      <div
        className="ios-panel relative flex h-full flex-col overflow-hidden rounded-2xl text-neutral-900 shadow-[0_18px_44px_-16px_rgba(0,0,0,0.58)] dark:text-neutral-100 dark:shadow-[0_20px_48px_-18px_rgba(0,0,0,0.82)]"
      >
        {/* The window has no title bar (decorations: false), so this is the
            only way to move it. `"deep"` (Tauri's own drag-region mode, not
            hand-rolled here) makes any non-interactive descendant of the
            header a drag handle — the icon, the count badge, the padding —
            while automatically excluding real controls like the search
            input, so a plain click there still just focuses it. Also needs
            core:window:allow-start-dragging in capabilities/default.json;
            Tauri v2 denies every window command that isn't explicitly
            allowlisted, silently, which is why this did nothing at first. */}
        <header
          ref={headerRef}
          data-tauri-drag-region="deep"
          className="ios-toolbar flex cursor-grab items-center gap-2 border-b border-black/[0.055] px-3 py-2.5 active:cursor-grabbing dark:border-white/[0.07]"
        >
          <GripHorizontal
            aria-hidden
            className="size-4 shrink-0 text-neutral-400/80"
            strokeWidth={1.8}
          />
          <div className="ios-search flex min-w-0 flex-1 items-center gap-2 rounded-xl bg-black/[0.055] px-3 py-2 dark:bg-white/[0.085]">
            <Search aria-hidden className="size-4 shrink-0 text-neutral-500 dark:text-neutral-400" strokeWidth={2} />
            <input
              ref={inputRef}
              value={query}
              onChange={(e) => {
                setQuery(e.target.value);
                setSelected(0);
                setEmojiSelected(0);
              }}
              placeholder={activeTab === "emoji" ? "Search emojis" : "Search clipboard"}
              spellCheck={false}
              autoComplete="off"
              className="w-full cursor-text bg-transparent text-[14px] outline-none placeholder:text-neutral-500 dark:placeholder:text-neutral-400"
            />
            {query && (
              <button
                type="button"
                onClick={() => {
                  setQuery("");
                  setSelected(0);
                  setEmojiSelected(0);
                  inputRef.current?.focus();
                }}
                aria-label="Clear search"
                title="Clear search"
                className="icon-button size-5 cursor-pointer"
              >
                <X className="size-3.5" strokeWidth={2.2} />
              </button>
            )}
          </div>
        </header>

        <nav
          aria-label="Content"
          className="ios-toolbar flex items-center border-b border-black/[0.05] px-3 py-2 dark:border-white/[0.06]"
        >
          <div className="flex rounded-xl bg-black/[0.055] p-0.5 dark:bg-white/[0.08]">
            <TabIcon
              active={activeTab === "clipboard"}
              label="Clipboard"
              onClick={() => changeTab("clipboard")}
            >
              <Clipboard className="size-4" />
            </TabIcon>
            <TabIcon
              active={activeTab === "emoji"}
              label="Emoji"
              onClick={() => changeTab("emoji")}
            >
              <SmilePlus className="size-4" />
            </TabIcon>
          </div>
          <span
            className="ml-auto flex items-center gap-1.5 text-[11px] tabular-nums text-neutral-500 dark:text-neutral-400"
            title={activeTab === "emoji" ? `${emojiMatches.length} emojis` : `${matches.length} clipboard items`}
          >
            <Layers3 aria-hidden className="size-3.5" strokeWidth={1.9} />
            {activeTab === "emoji" ? emojiMatches.length : matches.length}
          </span>
        </nav>

        <div className="flex min-h-0 flex-1">
          {activeTab === "emoji" ? (
            <Suspense fallback={<div className="flex-1" />}>
              <EmojiPicker
                group={emojiGroup}
                items={emojiMatches}
                selected={emojiSelected}
                onGroup={handleEmojiGroup}
                onPaste={pasteEmoji}
                tone={emojiTone}
                onTone={setEmojiTone}
              />
            </Suspense>
          ) : (
            <>
              <div
                ref={listRef}
                role="listbox"
                aria-label="Clipboard history"
                className="min-w-0 flex-1 overflow-y-auto overscroll-contain px-2 py-2"
              >
                {matches.length === 0 ? (
                  <Empty query={query} />
                ) : (
                  <div className="relative w-full" style={{ height: virtualizer.getTotalSize() }}>
                    {virtualizer.getVirtualItems().map((v) => (
                      <div
                        key={matches[v.index]!.item.id}
                        className="absolute inset-x-0 top-0"
                        style={{ height: v.size, transform: `translateY(${v.start}px)` }}
                      >
                        <Row
                          index={v.index}
                          match={matches[v.index]!}
                          active={v.index === selected}
                          now={now}
                          onActivate={handleActivateRow}
                        />
                      </div>
                    ))}
                  </div>
                )}
              </div>

              {current?.kind === "image" && <Preview item={current} onMessage={flash} />}
            </>
          )}
        </div>

        <Footer emoji={activeTab === "emoji"} />

        {/* Always mounted so the exit transition can actually play — a
            conditionally-rendered toast would just vanish, never fade. */}
        <div
          className={[
            "pointer-events-none absolute bottom-14 left-1/2 -translate-x-1/2 rounded-full bg-neutral-900/95 px-4 py-2 text-xs font-medium text-white shadow-lg transition-all duration-200 ease-out dark:bg-neutral-50/95 dark:text-neutral-900",
            toast ? "translate-y-0 scale-100 opacity-100" : "translate-y-1 scale-95 opacity-0",
          ].join(" ")}
        >
          {toast}
        </div>
      </div>
    </div>
  );
}

function Empty({ query }: { query: string }) {
  const EmptyIcon = query ? SearchX : ClipboardX;
  return (
    <div className="flex h-full flex-col items-center justify-center gap-2 px-6 text-center">
      <span aria-hidden className="grid size-11 place-items-center rounded-[14px] bg-black/[0.055] text-neutral-500 dark:bg-white/[0.08] dark:text-neutral-400">
        <EmptyIcon className="size-5" strokeWidth={1.8} />
      </span>
      <p className="text-sm font-medium text-neutral-600 dark:text-neutral-300">
        {query ? "No matches" : "Nothing copied yet"}
      </p>
      <p className="text-xs text-neutral-400">
        {query ? "Try a shorter search" : "Copy something and it will appear here"}
      </p>
    </div>
  );
}

function Footer({ emoji }: { emoji: boolean }) {
  return (
    <footer className="ios-toolbar flex items-center gap-1 border-t border-black/[0.055] px-3 py-1.5 text-neutral-500 dark:border-white/[0.07] dark:text-neutral-400">
      <Key icon={<ArrowUpDown />} k="↑↓" label="Move" />
      <Key icon={<CornerDownLeft />} k="↵" label={emoji ? "Paste emoji" : "Paste"} />
      {!emoji && <Key icon={<TextCursorInput />} k="⇧↵" label="Plain text" />}
      {!emoji && <Key icon={<Pin />} k="⌃P" label="Pin" />}
      {!emoji && <Key icon={<Trash2 />} k="Del" label="Delete" />}
      <Key icon={<X />} k="Esc" label="Close" />
      <span className="ml-auto grid size-7 place-items-center" title="GPL-3.0-or-later" aria-label="GPL-3.0-or-later license">
        <ShieldCheck className="size-3.5" strokeWidth={1.8} />
      </span>
    </footer>
  );
}

function Key({ icon, k, label }: { icon: ReactElement<{ className?: string }>; k: string; label: string }) {
  return (
    <span className="flex items-center gap-1.5 rounded-lg px-1.5 py-1" title={`${label} · ${k}`} aria-label={`${label}: ${k}`}>
      <span aria-hidden>{cloneElement(icon, { className: "size-3.5" })}</span>
      <span className="text-[10px] font-medium text-neutral-600 dark:text-neutral-300">{label}</span>
      <kbd
        className="rounded-md bg-black/[0.055] px-1.5 py-0.5 font-sans text-[9px] font-semibold text-neutral-600 dark:bg-white/[0.09] dark:text-neutral-300"
      >
        {k}
      </kbd>
    </span>
  );
}

function TabIcon({
  active,
  label,
  onClick,
  children,
}: {
  active: boolean;
  label: string;
  onClick: () => void;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      aria-label={label}
      title={label}
      aria-current={active ? "page" : undefined}
      className={[
        "grid size-8 place-items-center rounded-[10px] transition-[background-color,color,transform] duration-150 active:scale-[0.94]",
        active
          ? "bg-white text-[var(--accent)] shadow-[0_1px_4px_rgba(0,0,0,0.12)] dark:bg-white/[0.14] dark:text-blue-300"
          : "text-neutral-500 hover:bg-black/[0.04] dark:text-neutral-400 dark:hover:bg-white/[0.06]",
      ].join(" ")}
    >
      {children}
    </button>
  );
}
