import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useVirtualizer } from "@tanstack/react-virtual";
import { api, onDaemonEvent, onOpened, type Item } from "./api";
import { filterItems, mergeResults, type Match } from "./filter";
import { Row } from "./Row";
import { Preview } from "./Preview";

/** How many items the hot list holds. Tier-1 filtering stays instant well past this. */
const HOT_SIZE = 300;
const ROW_HEIGHT = 52;

export default function App() {
  const [items, setItems] = useState<Item[]>([]);
  const [query, setQuery] = useState("");
  const [cold, setCold] = useState<Item[]>([]);
  const [selected, setSelected] = useState(0);
  const [toast, setToast] = useState<string | null>(null);

  const inputRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  const panelRef = useRef<HTMLDivElement>(null);

  // ---- data ------------------------------------------------------------

  useEffect(() => {
    api.recent(HOT_SIZE).then(setItems).catch(() => setItems([]));
  }, []);

  // Keep the hot list current from the daemon's push stream. This is what lets
  // the popup open without querying anything.
  useEffect(() => {
    const un = onDaemonEvent((event) => {
      // Notices are messages, not list mutations. Only reachable from X11 now
      // — the daemon-side attempt at "hide, then let the daemon try Ctrl+V"
      // failed to restore focus (rare). Wayland has no daemon-side attempt at
      // all right now, so it never reaches this path; see `act`'s "paste"
      // case below for how it shows its own toast instead. Hiding afterward
      // is unconditional and safe even if the window is already hidden —
      // `hide()` on a hidden window is a no-op.
      if (event.event === "notice") {
        setToast(event.message);
        setTimeout(() => {
          setToast(null);
          void api.hide();
        }, 3200);
        return;
      }
      setItems((prev) => {
        switch (event.event) {
          case "added":
            return [event.item, ...prev.filter((i) => i.id !== event.item.id)].slice(0, HOT_SIZE);
          case "updated":
            return prev.map((i) => (i.id === event.item.id ? event.item : i));
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
  }, []);

  // Reset to a predictable state every time the popup opens. Deliberately not
  // preserving the previous query: reopening should always mean "top of list".
  useEffect(() => {
    const un = onOpened(() => {
      setQuery("");
      setCold([]);
      setSelected(0);
      listRef.current?.scrollTo({ top: 0 });
      inputRef.current?.focus();

      // The webview is never remounted between opens (that's the whole point
      // of pre-warming it), so a plain CSS animation on the panel would only
      // ever play once. Removing and re-adding the class forces a reflow in
      // between, which restarts it on every open instead.
      const el = panelRef.current;
      if (el) {
        el.classList.remove("pop-in");
        void el.offsetWidth;
        el.classList.add("pop-in");
      }
    });
    return () => {
      un.then((f) => f());
    };
  }, []);

  // Tier 2: the cold tail. Debounced, and merged in when it arrives — the user
  // is already looking at tier-1 results by then.
  useEffect(() => {
    if (!query.trim()) {
      setCold([]);
      return;
    }
    const t = setTimeout(() => {
      api.search(query, HOT_SIZE).then(setCold).catch(() => setCold([]));
    }, 60);
    return () => clearTimeout(t);
  }, [query]);

  const matches: Match[] = useMemo(() => {
    const hot = filterItems(query, items);
    return query.trim() ? mergeResults(hot, cold, query) : hot;
  }, [query, items, cold]);

  // Keep the selection in range as results change under it.
  useEffect(() => {
    setSelected((s) => Math.min(s, Math.max(0, matches.length - 1)));
  }, [matches.length]);

  // ---- virtualised list ------------------------------------------------

  const virtualizer = useVirtualizer({
    count: matches.length,
    getScrollElement: () => listRef.current,
    estimateSize: () => ROW_HEIGHT,
    overscan: 8,
  });

  useEffect(() => {
    if (matches.length) virtualizer.scrollToIndex(selected, { align: "auto" });
  }, [selected, matches.length, virtualizer]);

  const flash = useCallback((msg: string) => {
    setToast(msg);
    setTimeout(() => setToast(null), 1800);
  }, []);

  // ---- actions ---------------------------------------------------------

  const act = useCallback(
    async (index: number, action: "paste" | "plain" | "copy" | "delete" | "pin") => {
      const item = matches[index]?.item;
      if (!item) return;
      switch (action) {
        case "paste":
        case "plain": {
          // Checked fresh on every paste, not cached: whether auto-paste is
          // possible can change between one paste and the next (Wayland only
          // right now — auto-paste isn't implemented yet, so this is always
          // false there; kept dynamic for when it is).
          const auto = await api.canAutopaste();
          if (auto) {
            // The daemon needs focus back on the target window before it
            // presses Ctrl+V, so hide before asking it to paste.
            await api.hide();
            await api.paste(item.id, action === "plain");
          } else {
            // No daemon event is coming to tell us to close — auto-paste
            // was never attempted, so nothing will fire one. Set the
            // clipboard, say so, then close ourselves once that's been read.
            await api.paste(item.id, action === "plain");
            setToast("Copied — press Ctrl+V to paste");
            setTimeout(() => {
              setToast(null);
              void api.hide();
            }, 1100);
          }
          break;
        }
        case "copy":
          await api.copy(item.id);
          break;
        case "delete":
          await api.remove(item.id);
          flash("Deleted");
          break;
        case "pin":
          await api.setPinned(item.id, !item.pinned);
          break;
      }
    },
    [matches, flash],
  );

  // ---- keyboard --------------------------------------------------------

  const onKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      // Alt+1..9 pastes the Nth item outright. Bare digits stay available for
      // typing a search, which is the more common action by far.
      if (e.altKey && /^[1-9]$/.test(e.key)) {
        e.preventDefault();
        void act(Number(e.key) - 1, "paste");
        return;
      }

      switch (e.key) {
        case "Escape":
          e.preventDefault();
          void api.hide();
          break;
        case "ArrowDown":
          e.preventDefault();
          setSelected((s) => Math.min(s + 1, matches.length - 1));
          break;
        case "ArrowUp":
          e.preventDefault();
          setSelected((s) => Math.max(s - 1, 0));
          break;
        case "PageDown":
          e.preventDefault();
          setSelected((s) => Math.min(s + 8, matches.length - 1));
          break;
        case "PageUp":
          e.preventDefault();
          setSelected((s) => Math.max(s - 8, 0));
          break;
        case "Home":
          if (!query) {
            e.preventDefault();
            setSelected(0);
          }
          break;
        case "Enter":
          e.preventDefault();
          if (e.shiftKey) void act(selected, "plain");
          else if (e.ctrlKey) void act(selected, "copy");
          else void act(selected, "paste");
          break;
        case "Delete":
          e.preventDefault();
          void act(selected, "delete");
          break;
        case "p":
          if (e.ctrlKey) {
            e.preventDefault();
            void act(selected, "pin");
          }
          break;
      }
    },
    [act, matches.length, selected, query],
  );

  // The search field owns focus permanently, so every keystroke filters
  // without the user ever reaching for Ctrl+F.
  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  const current = matches[selected]?.item;

  return (
    // Transparent inset around the panel: this is what the rounded corners
    // and shadow actually show against, now that the window itself has no
    // background of its own (tauri.conf.json's `transparent: true`).
    <div className="h-screen w-full p-3" onKeyDown={onKeyDown}>
      <div
        ref={panelRef}
        className="pop-in relative flex h-full flex-col overflow-hidden rounded-2xl border border-black/10 bg-neutral-50 text-neutral-900 shadow-[0_20px_50px_-12px_rgba(0,0,0,0.45),0_0_0_0.5px_rgba(0,0,0,0.06)] dark:border-white/10 dark:bg-neutral-900 dark:text-neutral-100 dark:shadow-[0_20px_50px_-12px_rgba(0,0,0,0.7),0_0_0_0.5px_rgba(255,255,255,0.06)]"
      >
        <header className="flex items-center gap-2.5 border-b border-black/[0.06] px-4 py-3 dark:border-white/[0.06]">
          <SearchIcon />
          <input
            ref={inputRef}
            value={query}
            onChange={(e) => {
              setQuery(e.target.value);
              setSelected(0);
            }}
            placeholder="Search clipboard history…"
            spellCheck={false}
            autoComplete="off"
            className="w-full bg-transparent text-[15px] outline-none placeholder:text-neutral-400"
          />
          <span className="shrink-0 rounded-full bg-black/5 px-2 py-0.5 text-[11px] font-medium tabular-nums text-neutral-500 dark:bg-white/10 dark:text-neutral-400">
            {matches.length}
          </span>
        </header>

        <div className="flex min-h-0 flex-1">
          <div ref={listRef} className="min-w-0 flex-1 overflow-y-auto overscroll-contain px-2 py-2">
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
                      match={matches[v.index]!}
                      active={v.index === selected}
                      onSelect={() => setSelected(v.index)}
                      onActivate={() => act(v.index, "paste")}
                    />
                  </div>
                ))}
              </div>
            )}
          </div>

          {current && <Preview item={current} />}
        </div>

        <Footer />

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
  return (
    <div className="flex h-full flex-col items-center justify-center gap-2 px-6 text-center">
      <span
        aria-hidden
        className="grid size-10 place-items-center rounded-xl bg-black/5 text-lg dark:bg-white/10"
      >
        {query ? "🔍" : "📋"}
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

function Footer() {
  return (
    <footer className="flex items-center gap-4 border-t border-black/[0.06] px-4 py-2 text-[11px] text-neutral-500 dark:border-white/[0.06] dark:text-neutral-400">
      <Key k="↑↓" label="Navigate" />
      <Key k="Enter" label="Paste" />
      <Key k="⇧Enter" label="Plain" />
      <Key k="Ctrl+P" label="Pin" />
      <Key k="Del" label="Remove" />
      <Key k="Esc" label="Close" />
    </footer>
  );
}

function Key({ k, label }: { k: string; label: string }) {
  return (
    <span className="flex items-center gap-1.5">
      <kbd
        className={[
          "rounded-[5px] border px-1.5 py-px font-sans text-[10px] font-medium",
          "border-black/10 bg-white text-neutral-600 shadow-[0_1px_0_rgba(0,0,0,0.06)]",
          "dark:border-white/10 dark:bg-white/10 dark:text-neutral-300 dark:shadow-[0_1px_0_rgba(0,0,0,0.3)]",
        ].join(" ")}
      >
        {k}
      </kbd>
      {label}
    </span>
  );
}

function SearchIcon() {
  return (
    <svg
      viewBox="0 0 20 20"
      className="size-4 shrink-0 text-neutral-400"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.6"
      aria-hidden
    >
      <circle cx="9" cy="9" r="6" />
      <path d="m13.5 13.5 3.5 3.5" strokeLinecap="round" />
    </svg>
  );
}
