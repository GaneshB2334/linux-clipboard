// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Ganesh Bastapure

import emojiData from "emojibase-data/en/data.json";

export type EmojiEntry = (typeof emojiData)[number];
export type EmojiTone = 0 | 1 | 2 | 3 | 4 | 5;

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

const searchable = emojiData.filter(
  (entry) => entry.type === 1 && entry.emoji && entry.tone === undefined,
);

export function emojiForTone(entry: EmojiEntry, tone: EmojiTone): string {
  if (tone === 0) return entry.emoji;
  return entry.skins?.find((skin) => skin.tone === tone)?.emoji ?? entry.emoji;
}

export function filterEmojis(query: string, group: number | null, recent: string[] = []): EmojiEntry[] {
  const normalized = query.trim().toLowerCase();
  return searchable
    .filter((entry) => {
      if (group === null) return true;
      if (group === -1) return recent.some((emoji) => emojiForTone(entry, 0) === emoji || entry.skins?.some((skin) => skin.emoji === emoji));
      return entry.group === group;
    })
    .filter((entry) => {
      if (!normalized) return true;
      const haystack = [entry.label, ...(entry.tags ?? []), ...(entry.shortcodes ?? [])]
        .join(" ")
        .toLowerCase();
      return haystack.includes(normalized);
    })
    .slice(0, 400);
}
