import type { Item } from "./api";

/**
 * Tier-1 search: fuzzy subsequence matching over the in-memory hot list.
 *
 * Runs synchronously on every keystroke because it costs microseconds over a
 * few hundred items. The daemon's trigram FTS covers the cold tail and is
 * merged in when it lands, so the user never waits and never sees a spinner.
 */

export type Match = { item: Item; score: number; ranges: [number, number][] };

/**
 * Score a candidate, or return null if it doesn't match.
 *
 * Scoring rewards, in order: a contiguous substring hit, a hit at a word
 * boundary, and an early hit. This puts "npm install" above "n...p...m" for the
 * query "npm", which is what makes the ranking feel obvious rather than clever.
 */
export function fuzzyMatch(query: string, text: string): Omit<Match, "item"> | null {
  if (!query) return { score: 0, ranges: [] };

  const q = query.toLowerCase();
  const t = text.toLowerCase();

  // Contiguous substring: the common case, and always the best kind of match.
  const direct = t.indexOf(q);
  if (direct !== -1) {
    const boundary = direct === 0 || /[\s/\-_.:]/.test(t[direct - 1]!);
    return {
      score: 1000 - direct + (boundary ? 500 : 0),
      ranges: [[direct, direct + q.length]],
    };
  }

  // Subsequence fallback, so "nir" still finds "npm install react".
  const ranges: [number, number][] = [];
  let ti = 0;
  let score = 0;
  let runStart = -1;
  for (const ch of q) {
    const found = t.indexOf(ch, ti);
    if (found === -1) return null;
    if (runStart === -1) runStart = found;
    else if (found !== ti) {
      ranges.push([runStart, ti]);
      runStart = found;
    }
    // Adjacent characters are worth more than scattered ones.
    score += found === ti ? 8 : 1;
    if (found === 0 || /[\s/\-_.:]/.test(t[found - 1]!)) score += 4;
    ti = found + 1;
  }
  if (runStart !== -1) ranges.push([runStart, ti]);
  return { score, ranges };
}

export function filterItems(query: string, items: Item[]): Match[] {
  if (!query.trim()) return items.map((item) => ({ item, score: 0, ranges: [] }));

  const out: Match[] = [];
  for (const item of items) {
    // Secrets are never searchable — matching them would defeat the masking.
    if (item.sensitive) continue;
    const m = fuzzyMatch(query, item.preview);
    if (m) out.push({ item, ...m });
  }

  // Pinned first, then score, then recency — a stable, predictable order.
  out.sort((a, b) => {
    if (a.item.pinned !== b.item.pinned) return a.item.pinned ? -1 : 1;
    if (b.score !== a.score) return b.score - a.score;
    return Number(b.item.last_used_at) - Number(a.item.last_used_at);
  });
  return out;
}

/** Merge cold-tail FTS results into hot-list results without duplicating. */
export function mergeResults(hot: Match[], cold: Item[], query: string): Match[] {
  const seen = new Set(hot.map((m) => m.item.id));
  const extra: Match[] = [];
  for (const item of cold) {
    if (seen.has(item.id) || item.sensitive) continue;
    const m = fuzzyMatch(query, item.preview);
    extra.push({ item, score: m?.score ?? 0, ranges: m?.ranges ?? [] });
  }
  return [...hot, ...extra];
}
