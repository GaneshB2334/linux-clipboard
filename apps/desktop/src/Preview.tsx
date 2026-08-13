import type { Item } from "./api";

/**
 * Detail pane for the selected row.
 *
 * Exists so the list can stay dense: truncated one-liners are fine when the
 * full content is always visible next to them, and it removes the guesswork
 * about which of three similar-looking items is the one you want.
 */
export function Preview({ item }: { item: Item }) {
  return (
    <aside className="hidden w-[280px] shrink-0 flex-col gap-3 overflow-y-auto border-l border-black/[0.06] p-4 sm:flex dark:border-white/[0.06]">
      {item.sensitive ? (
        <div className="rounded-xl border border-amber-300/50 bg-amber-50 p-3 text-xs text-amber-900 dark:border-amber-400/20 dark:bg-amber-500/10 dark:text-amber-200">
          <strong className="block">Hidden</strong>
          This looked like a credential, so it was never written to disk or
          indexed for search.
        </div>
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

      <dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 text-[11px] text-neutral-500">
        <Field label="Type" value={item.kind} />
        <Field label="Size" value={bytes(Number(item.byte_size))} />
        <Field label="Copied" value={when(Number(item.created_at))} />
        {item.source_app && <Field label="From" value={item.source_app} />}
        {item.use_count > 1 && <Field label="Used" value={`${item.use_count} times`} />}
        {item.mimes.length > 1 && <Field label="Formats" value={item.mimes.join(", ")} />}
      </dl>
    </aside>
  );
}

function Field({ label, value }: { label: string; value: string }) {
  return (
    <>
      <dt className="text-neutral-400">{label}</dt>
      <dd className="truncate text-neutral-600 dark:text-neutral-300" title={value}>
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
