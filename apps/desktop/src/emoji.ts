// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Ganesh Bastapure

import emojiData from "emojibase-data/en/data.json";

export type EmojiEntry = (typeof emojiData)[number];
export type EmojiTone = 0 | 1 | 2 | 3 | 4 | 5;

/** Fixed column count for the emoji grid and Up/Down keyboard navigation. */
export const EMOJI_COLS = 10;

export const EMOJI_GROUPS = [
  { id: -1, label: "Recent" },
  { id: null, label: "All" },
  { id: 0, label: "Smileys" },
  { id: 1, label: "People" },
  { id: 3, label: "Nature" },
  { id: 4, label: "Food" },
  { id: 5, label: "Places" },
  { id: 6, label: "Activities" },
  { id: 7, label: "Objects" },
  { id: 8, label: "Symbols" },
  { id: 9, label: "Flags" },
] as const;

type IndexedEmoji = {
  entry: EmojiEntry;
  haystack: string;
};

let indexedCache: IndexedEmoji[] | null = null;
let byEmojiCharCache: Map<string, IndexedEmoji> | null = null;

/** Lazily index emoji search haystacks once so startup stays free and typing is O(N) substring checks. */
function getIndex(): { list: IndexedEmoji[]; byChar: Map<string, IndexedEmoji> } {
  if (indexedCache && byEmojiCharCache) {
    return { list: indexedCache, byChar: byEmojiCharCache };
  }
  const list: IndexedEmoji[] = [];
  const byChar = new Map<string, IndexedEmoji>();
  for (const entry of emojiData) {
    if (entry.type !== 1 || !entry.emoji || entry.tone !== undefined) continue;
    const haystack = [entry.label, ...(entry.tags ?? []), ...(entry.shortcodes ?? [])]
      .join(" ")
      .toLowerCase();
    const record: IndexedEmoji = { entry, haystack };
    list.push(record);
    byChar.set(entry.emoji, record);
    if (entry.skins) {
      for (const skin of entry.skins) {
        if (skin.emoji) byChar.set(skin.emoji, record);
      }
    }
  }
  indexedCache = list;
  byEmojiCharCache = byChar;
  return { list, byChar };
}

export function emojiForTone(entry: EmojiEntry, tone: EmojiTone): string {
  if (tone === 0) return entry.emoji;
  return entry.skins?.find((skin) => skin.tone === tone)?.emoji ?? entry.emoji;
}

export function filterEmojis(query: string, group: number | null, recent: string[] = []): EmojiEntry[] {
  const { list, byChar } = getIndex();
  const normalized = query.trim().toLowerCase();
  const results: EmojiEntry[] = [];

  if (group === -1) {
    // Preserve most-recently-used order and deduplicate skin-tone variants.
    const seen = new Set<string>();
    for (const ch of recent) {
      const record = byChar.get(ch);
      if (!record || seen.has(record.entry.hexcode)) continue;
      if (normalized && !record.haystack.includes(normalized)) continue;
      seen.add(record.entry.hexcode);
      results.push(record.entry);
      if (results.length >= 400) break;
    }
    return results;
  }

  for (const record of list) {
    if (group !== null && record.entry.group !== group) continue;
    if (normalized && !record.haystack.includes(normalized)) continue;
    results.push(record.entry);
    if (results.length >= 400) break;
  }
  return results;
}
