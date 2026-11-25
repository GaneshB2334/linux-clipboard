// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Ganesh Bastapure

import { useEffect, useState } from "react";
import {
  AppWindow,
  CalendarClock,
  Database,
  EyeOff,
  FileType2,
  History,
  Image as ImageIcon,
  LoaderCircle,
  type LucideIcon,
} from "lucide-react";
import type { Item } from "./api";
import { api, getCachedThumbnail } from "./api";

/**
 * Detail pane for the selected image row — shows a larger preview along with
 * dimensions, size, and copy metadata.
 */
export function Preview({ item }: { item: Item }) {
  const [imageSrc, setImageSrc] = useState(() => getCachedThumbnail(item.id) ?? "");
  const [loading, setLoading] = useState(
    () => item.kind === "image" && item.has_thumbnail && !getCachedThumbnail(item.id),
  );

  useEffect(() => {
    const cached = getCachedThumbnail(item.id);
    if (cached !== undefined) {
      setImageSrc(cached);
      setLoading(false);
      return;
    }

    setImageSrc("");
    if (item.kind !== "image" || !item.has_thumbnail) {
      setLoading(false);
      return;
    }

    let alive = true;
    setLoading(true);
    api
      .thumbnail(item.id)
      .then((value) => {
        if (alive) setImageSrc(value);
      })
      .catch(() => undefined)
      .finally(() => {
        if (alive) setLoading(false);
      });
    return () => {
      alive = false;
    };
  }, [item.id, item.kind, item.has_thumbnail]);

  const typeLabel =
    item.kind === "html" || (item.kind === "text" && item.mimes.includes("text/html"))
      ? "rich text"
      : item.kind;

  return (
    <aside className="ios-preview hidden w-[310px] shrink-0 flex-col gap-3 overflow-y-auto border-l border-black/[0.055] p-3.5 sm:flex dark:border-white/[0.07]">
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
          <div className="flex flex-1 min-h-[180px] items-center justify-center overflow-hidden rounded-xl bg-black/[0.04] p-1.5 dark:bg-white/[0.055]">
            {imageSrc ? (
              <img
                src={imageSrc}
                alt="Clipboard preview"
                className="max-h-[260px] w-full rounded-lg object-contain"
              />
            ) : loading ? (
              <div className="grid h-44 place-items-center text-neutral-400" aria-label="Loading preview">
                <LoaderCircle className="size-5 animate-spin" strokeWidth={1.8} />
              </div>
            ) : (
              <div className="grid h-44 place-items-center text-neutral-400" aria-label="No preview available">
                <ImageIcon className="size-6" strokeWidth={1.8} />
              </div>
            )}
          </div>
          <div className="flex items-center gap-1.5 text-xs text-neutral-500 dark:text-neutral-400">
            <ImageIcon className="size-3.5" strokeWidth={1.8} />
            {item.image_width && item.image_height
              ? `${item.image_width} × ${item.image_height} · `
              : ""}
            {item.image_format?.replace("image/", "").toUpperCase() ?? "IMAGE"}
          </div>
        </>
      ) : (
        <p
          className={[
            "max-h-56 overflow-y-auto whitespace-pre-wrap break-words text-xs leading-relaxed",
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

      <dl className="grid grid-cols-[20px_1fr] items-center gap-x-2 gap-y-1.5 text-[11px] text-neutral-500">
        <Field icon={FileType2} label="Type" value={typeLabel} />
        <Field icon={Database} label="Size" value={bytes(Number(item.byte_size))} />
        <Field icon={CalendarClock} label="Copied" value={when(Number(item.created_at))} />
        {item.source_app && <Field icon={AppWindow} label="From" value={item.source_app} />}
        {item.use_count > 1 && <Field icon={History} label="Used" value={`${item.use_count} times`} />}
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
