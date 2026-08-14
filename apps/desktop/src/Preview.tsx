// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Ganesh Bastapure

import { useEffect, useState } from "react";
import {
  AppWindow,
  CalendarClock,
  Copy,
  Database,
  EyeOff,
  FileType2,
  History,
  Image as ImageIcon,
  Layers3,
  LoaderCircle,
  Scaling,
  type LucideIcon,
} from "lucide-react";
import type { Item } from "./api";
import { api } from "./api";

/**
 * Detail pane for the selected row.
 *
 * Exists so the list can stay dense: truncated one-liners are fine when the
 * full content is always visible next to them, and it removes the guesswork
 * about which of three similar-looking items is the one you want.
 */
export function Preview({ item, onMessage }: { item: Item; onMessage?: (message: string) => void }) {
  const [imageSrc, setImageSrc] = useState("");
  const [customWidth, setCustomWidth] = useState("");
  const [customHeight, setCustomHeight] = useState("");
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    let alive = true;
    setImageSrc("");
    if (item.kind === "image" && item.has_thumbnail) {
      api.thumbnail(item.id).then((value) => {
        if (alive) setImageSrc(value);
      }).catch(() => undefined);
    }
    return () => {
      alive = false;
    };
  }, [item.id, item.kind, item.has_thumbnail]);

  const resize = async (width: number, height?: number) => {
    setBusy(true);
    try {
      await api.resizeCopy(item.id, width, height);
      onMessage?.(height ? `Copied resized image at ${width} × ${height}px` : `Copied resized image at ${width}px`);
    } catch (error) {
      onMessage?.(String(error));
    } finally {
      setBusy(false);
    }
  };

  return (
    <aside className="ios-preview hidden w-[280px] shrink-0 flex-col gap-3 overflow-y-auto border-l border-black/[0.055] p-4 sm:flex dark:border-white/[0.07]">
      {item.sensitive ? (
        <div className="flex gap-2.5 rounded-xl bg-amber-500/10 p-3 text-xs text-amber-900 dark:text-amber-200">
          <EyeOff className="mt-0.5 size-4 shrink-0" strokeWidth={1.8} />
          <div>
            <strong className="block">Hidden</strong>
            This looked like a credential, so it was never written to disk or
            indexed for search.
          </div>
        </div>
      ) : item.kind === "image" ? (
        <>
          <div className="overflow-hidden rounded-xl bg-black/[0.04] dark:bg-white/[0.055]">
            {imageSrc ? (
              <img src={imageSrc} alt="Clipboard preview" className="max-h-48 w-full object-contain" />
            ) : (
              <div className="grid h-32 place-items-center text-neutral-400" aria-label="Loading preview">
                <LoaderCircle className="size-5 animate-spin" strokeWidth={1.8} />
              </div>
            )}
          </div>
          <div className="flex items-center gap-1.5 text-xs text-neutral-500">
            <ImageIcon className="size-3.5" strokeWidth={1.8} />
            {item.image_width} × {item.image_height} · {item.image_format?.replace("image/", "").toUpperCase()}
          </div>
        </>
      ) : (
        <p
          className={[
            "max-h-48 overflow-y-auto whitespace-pre-wrap break-words text-xs leading-relaxed",
            item.kind === "code" ? "font-mono" : "",
          ].join(" ")}
        >
          {item.preview}
        </p>
      )}

      {item.kind === "color" && (
        <div
          className="h-12 w-full rounded-xl ring-1 ring-black/10"
          style={{ background: item.preview }}
        />
      )}

      {item.kind === "image" && (
        <div className="rounded-xl bg-black/[0.035] p-3 dark:bg-white/[0.05]">
          <div className="mb-2 flex items-center gap-1.5 text-[11px] font-medium text-neutral-600 dark:text-neutral-300">
            <Scaling className="size-3.5" strokeWidth={1.8} />
            Resize & copy
          </div>
          <div className="grid grid-cols-2 gap-1.5">
            {[25, 50].map((percent) => (
              <button
                key={percent}
                type="button"
                disabled={busy || !item.image_width}
                onClick={() => resize(Math.max(1, Math.round((item.image_width ?? 1) * percent / 100)))}
                className="rounded-lg bg-black/[0.05] px-2 py-1.5 text-[11px] text-neutral-600 transition hover:bg-black/[0.1] disabled:opacity-40 dark:bg-white/[0.08] dark:text-neutral-300 dark:hover:bg-white/[0.14]"
              >
                {percent}%
              </button>
            ))}
            {[1024, 1920].map((width) => (
              <button
                key={width}
                type="button"
                disabled={busy || !item.image_width}
                onClick={() => resize(Math.min(width, item.image_width ?? width))}
                className="rounded-lg bg-black/[0.05] px-2 py-1.5 text-[11px] text-neutral-600 transition hover:bg-black/[0.1] disabled:opacity-40 dark:bg-white/[0.08] dark:text-neutral-300 dark:hover:bg-white/[0.14]"
              >
                {width}px
              </button>
            ))}
          </div>
          <div className="mt-2 flex items-center gap-1.5 text-[10px] text-neutral-500 dark:text-neutral-400" title="Leave height empty to preserve aspect ratio">
            <Scaling className="size-3" strokeWidth={1.8} />
            Custom size
          </div>
          <div className="mt-1.5 flex gap-1.5">
            <input
              value={customWidth}
              onChange={(event) => setCustomWidth(event.target.value.replace(/\D/g, ""))}
              placeholder="Width"
              inputMode="numeric"
              className="min-w-0 flex-1 rounded-lg bg-black/[0.05] px-2 py-1.5 text-[11px] outline-none ring-[var(--accent)] focus:ring-1 dark:bg-white/[0.08]"
            />
            <input
              value={customHeight}
              onChange={(event) => setCustomHeight(event.target.value.replace(/\D/g, ""))}
              placeholder="Height"
              inputMode="numeric"
              className="min-w-0 flex-1 rounded-lg bg-black/[0.05] px-2 py-1.5 text-[11px] outline-none ring-[var(--accent)] focus:ring-1 dark:bg-white/[0.08]"
            />
            <button
              type="button"
              disabled={busy || !customWidth}
              onClick={() => resize(Number(customWidth), customHeight ? Number(customHeight) : undefined)}
              title="Resize and copy"
              aria-label="Resize and copy"
              className="grid size-7 place-items-center rounded-lg bg-[var(--accent)] text-white transition-transform active:scale-[0.92] disabled:opacity-40"
            >
              <Copy className="size-3.5" strokeWidth={2} />
            </button>
          </div>
        </div>
      )}

      <dl className="grid grid-cols-[20px_1fr] items-center gap-x-2 gap-y-1.5 text-[11px] text-neutral-500">
        <Field icon={FileType2} label="Type" value={item.kind} />
        <Field icon={Database} label="Size" value={bytes(Number(item.byte_size))} />
        <Field icon={CalendarClock} label="Copied" value={when(Number(item.created_at))} />
        {item.source_app && <Field icon={AppWindow} label="From" value={item.source_app} />}
        {item.use_count > 1 && <Field icon={History} label="Used" value={`${item.use_count} times`} />}
        {item.mimes.length > 1 && <Field icon={Layers3} label="Formats" value={item.mimes.join(", ")} />}
      </dl>
    </aside>
  );
}

function Field({ icon: Icon, label, value }: { icon: LucideIcon; label: string; value: string }) {
  return (
    <>
      <dt className="grid size-5 place-items-center text-neutral-400" title={label} aria-label={label}>
        <Icon className="size-3.5" strokeWidth={1.8} />
      </dt>
      <dd className="truncate text-neutral-600 dark:text-neutral-300" title={`${label}: ${value}`}>
        {value}
      </dd>
    </>
  );
}

function bytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / 1024 / 1024).toFixed(1)} MB`;
}

function when(ms: number): string {
  return new Date(ms).toLocaleString(undefined, {
    dateStyle: "medium",
    timeStyle: "short",
  });
}
