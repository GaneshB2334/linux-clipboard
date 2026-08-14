// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Ganesh Bastapure

import { useEffect, useState } from "react";
import {
  AppWindow,
  Clock3,
  Code2,
  FileCode2,
  FileText,
  Folder,
  Globe2,
  History,
  Image as ImageIcon,
  Palette,
  Pin,
  type LucideIcon,
} from "lucide-react";
import type { Match } from "./filter";
import { api, type Item } from "./api";

const ICONS: Record<Item["kind"], LucideIcon> = {
  text: FileText,
  url: Globe2,
  code: Code2,
  color: Palette,
  html: FileCode2,
  image: ImageIcon,
  files: Folder,
};

export function Row({
  match,
  active,
  onSelect,
  onActivate,
}: {
  match: Match;
  active: boolean;
  onSelect: () => void;
  onActivate: () => void;
}) {
  const { item, ranges } = match;
  const KindIcon = ICONS[item.kind];

  return (
    <div
      role="option"
      aria-selected={active}
      onMouseMove={onSelect}
      onClick={onActivate}
      className={[
        "mx-1 flex h-[56px] cursor-default items-center gap-3 rounded-xl px-2.5 transition-[background-color,transform] duration-150 active:scale-[0.995]",
        active
          ? "bg-[oklch(0.62_0.16_255/13%)] dark:bg-[oklch(0.7_0.14_255/17%)]"
          : "hover:bg-black/[0.035] dark:hover:bg-white/[0.045]",
      ].join(" ")}
    >
      <span
        aria-hidden
        className={[
          "grid shrink-0 place-items-center overflow-hidden rounded-xl transition-colors",
          item.kind === "image" ? "h-10 w-14" : "size-9",
          active
            ? "bg-white/70 text-[var(--accent)] dark:bg-white/[0.1] dark:text-blue-300"
            : "bg-black/[0.045] text-neutral-500 dark:bg-white/[0.065] dark:text-neutral-400",
        ].join(" ")}
      >
        {item.kind === "image" ? (
          <Thumbnail item={item} />
        ) : item.kind === "color" ? (
          <span
            className="size-4 rounded-sm ring-1 ring-black/10"
            style={{ background: item.preview }}
          />
        ) : (
          <KindIcon className="size-[17px]" strokeWidth={1.8} />
        )}
      </span>

      <div className="min-w-0 flex-1">
        <div
          className={[
            "truncate text-sm",
            item.sensitive ? "select-none blur-[5px]" : "",
            item.kind === "code" ? "font-mono text-[13px]" : "",
          ].join(" ")}
        >
          {item.sensitive ? "•••••••••••••••" : <Highlight text={item.preview} ranges={ranges} />}
        </div>
        <div className="mt-0.5 flex items-center gap-2.5 text-[10px] text-neutral-500 dark:text-neutral-400">
          <span className="flex shrink-0 items-center gap-1" title="Last copied">
            <Clock3 aria-hidden className="size-3" strokeWidth={1.8} />
            {ago(Number(item.last_used_at))}
          </span>
          {item.source_app && (
            <span className="flex min-w-0 items-center gap-1" title={`Copied from ${item.source_app}`}>
              <AppWindow aria-hidden className="size-3 shrink-0" strokeWidth={1.8} />
              <span className="truncate">{item.source_app}</span>
            </span>
          )}
          {item.use_count > 1 && (
            <span className="flex shrink-0 items-center gap-1" title={`Used ${item.use_count} times`}>
              <History aria-hidden className="size-3" strokeWidth={1.8} />
              {item.use_count}×
            </span>
          )}
        </div>
      </div>

      {item.pinned && (
        <span className="grid size-7 shrink-0 place-items-center text-amber-500" title="Pinned" aria-label="Pinned">
          <Pin className="size-3.5 fill-current" strokeWidth={1.8} />
        </span>
      )}
    </div>
  );
}

function Thumbnail({ item }: { item: Item }) {
  const [src, setSrc] = useState("");

  useEffect(() => {
    let alive = true;
    if (item.has_thumbnail) {
      api.thumbnail(item.id).then((value) => {
        if (alive) setSrc(value);
      }).catch(() => undefined);
    }
    return () => {
      alive = false;
    };
  }, [item.id, item.has_thumbnail]);

  return src ? (
    <img src={src} alt="" className="h-10 w-14 rounded-xl object-cover" />
  ) : (
    <ImageIcon aria-hidden className="size-[17px]" strokeWidth={1.8} />
  );
}

/** Highlight the matched ranges so it's obvious *why* a row matched. */
function Highlight({ text, ranges }: { text: string; ranges: [number, number][] }) {
  if (!ranges.length) return <>{text}</>;

  const parts: React.ReactNode[] = [];
  let cursor = 0;
  ranges.forEach(([start, end], i) => {
    if (start > cursor) parts.push(text.slice(cursor, start));
    parts.push(
      <mark key={i} className="rounded-[2px] bg-amber-300/60 text-inherit dark:bg-amber-400/40">
        {text.slice(start, end)}
      </mark>,
    );
    cursor = end;
  });
  if (cursor < text.length) parts.push(text.slice(cursor));
  return <>{parts}</>;
}

function ago(ms: number): string {
  const s = Math.max(0, Math.floor((Date.now() - ms) / 1000));
  if (s < 60) return "just now";
  if (s < 3600) return `${Math.floor(s / 60)}m ago`;
  if (s < 86400) return `${Math.floor(s / 3600)}h ago`;
  return `${Math.floor(s / 86400)}d ago`;
}
