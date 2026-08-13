import type { Match } from "./filter";
import type { Item } from "./api";

const ICONS: Record<Item["kind"], string> = {
  text: "📄",
  url: "🌐",
  code: "💻",
  color: "🎨",
  html: "📝",
  image: "🖼",
  files: "📁",
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

  return (
    <div
      role="option"
      aria-selected={active}
      onMouseMove={onSelect}
      onClick={onActivate}
      className={[
        "mx-1 flex h-[52px] cursor-default items-center gap-3 rounded-xl px-3 transition-colors duration-100",
        active ? "bg-[oklch(0.62_0.19_255/12%)] dark:bg-[oklch(0.7_0.17_255/16%)]" : "",
      ].join(" ")}
    >
      <span
        aria-hidden
        className="grid size-8 shrink-0 place-items-center rounded-[10px] bg-black/[0.04] text-sm dark:bg-white/[0.06]"
      >
        {item.kind === "color" ? (
          <span
            className="size-4 rounded-sm ring-1 ring-black/10"
            style={{ background: item.preview }}
          />
        ) : (
          ICONS[item.kind]
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
        <div className="flex items-center gap-2 text-[11px] text-neutral-400">
          <span>{ago(Number(item.last_used_at))}</span>
          {item.source_app && <span className="truncate">· {item.source_app}</span>}
          {item.use_count > 1 && <span>· used {item.use_count}×</span>}
        </div>
      </div>

      {item.pinned && (
        <span className="shrink-0 text-xs text-amber-500" title="Pinned">
          ★
        </span>
      )}
    </div>
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
