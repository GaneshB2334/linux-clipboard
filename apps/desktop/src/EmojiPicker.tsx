// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 Ganesh Bastapure

import { emojiForTone, type EmojiEntry, type EmojiTone } from "./emoji";
import { EMOJI_GROUPS } from "./emoji";
import {
  CircleDashed,
  Clock3,
  Flag,
  Grid2X2,
  Hand,
  Leaf,
  Lightbulb,
  MapPin,
  SearchX,
  Shapes,
  Smile,
  Trophy,
  UtensilsCrossed,
  UsersRound,
  type LucideIcon,
} from "lucide-react";

const GROUP_ICONS: Record<string, LucideIcon> = {
  "-1": Clock3,
  all: Grid2X2,
  "0": Smile,
  "1": UsersRound,
  "3": Leaf,
  "4": UtensilsCrossed,
  "5": MapPin,
  "6": Trophy,
  "7": Lightbulb,
  "8": Shapes,
  "9": Flag,
};

export function EmojiPicker({
  group,
  items,
  selected,
  onGroup,
  onSelect,
  onPaste,
  tone,
  onTone,
}: {
  group: number | null;
  items: EmojiEntry[];
  selected: number;
  onGroup: (group: number | null) => void;
  onSelect: (index: number) => void;
  onPaste: (emoji: string) => void;
  tone: EmojiTone;
  onTone: (tone: EmojiTone) => void;
}) {
  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <div className="ios-toolbar flex items-center gap-1 overflow-x-auto border-b border-black/[0.05] px-3 py-2 dark:border-white/[0.06]">
        {EMOJI_GROUPS.map((category) => {
          const active = category.id === group;
          const CategoryIcon = GROUP_ICONS[category.id === null ? "all" : String(category.id)] ?? Grid2X2;
          return (
            <button
              key={category.label}
              type="button"
              onClick={() => onGroup(category.id)}
              title={category.label}
              aria-label={category.label}
              aria-pressed={active}
              className={[
                "grid size-8 shrink-0 place-items-center rounded-[10px] transition-[background-color,color,transform] duration-150 active:scale-[0.93]",
                active
                  ? "bg-[var(--accent)] text-white shadow-[0_2px_6px_oklch(0.5_0.15_255/24%)]"
                  : "text-neutral-500 hover:bg-black/[0.05] dark:text-neutral-400 dark:hover:bg-white/[0.08]",
              ].join(" ")}
            >
              <CategoryIcon className="size-4" strokeWidth={1.9} />
            </button>
          );
        })}
      </div>

      <div className="ios-toolbar flex items-center gap-1 border-b border-black/[0.04] px-3 py-1.5 dark:border-white/[0.04]">
        <span className="mr-1 grid size-6 place-items-center text-neutral-500 dark:text-neutral-400" title="Skin tone" aria-label="Skin tone">
          <Hand className="size-3.5" strokeWidth={1.8} />
        </span>
        {([0, 1, 2, 3, 4, 5] as const).map((value) => (
          <button
            key={value}
            type="button"
            title={value === 0 ? "Default skin tone" : `Skin tone ${value}`}
            aria-label={value === 0 ? "Default skin tone" : `Skin tone ${value}`}
            aria-pressed={tone === value}
            onClick={() => onTone(value)}
            className={[
              "grid size-6 place-items-center rounded-full text-sm transition-colors",
              tone === value ? "bg-white text-neutral-800 shadow-[0_1px_4px_rgba(0,0,0,0.14)] dark:bg-white/[0.14] dark:text-white" : "hover:bg-black/[0.05] dark:hover:bg-white/[0.08]",
            ].join(" ")}
          >
            {value === 0 ? <CircleDashed className="size-3.5" strokeWidth={1.8} /> : ["🏻", "🏼", "🏽", "🏾", "🏿"][value - 1]}
          </button>
        ))}
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto px-3 py-3">
        {items.length === 0 ? (
          <div className="flex h-full flex-col items-center justify-center gap-2 text-neutral-500 dark:text-neutral-400">
            <span className="grid size-11 place-items-center rounded-[14px] bg-black/[0.05] dark:bg-white/[0.07]">
              <SearchX className="size-5" strokeWidth={1.8} />
            </span>
            <span className="text-xs">No emojis found</span>
          </div>
        ) : (
          <div className="grid grid-cols-8 gap-1 sm:grid-cols-10">
            {items.map((item, index) => (
              <button
                key={item.hexcode}
                type="button"
                title={item.label}
                aria-label={item.label}
                aria-selected={index === selected}
                onMouseMove={() => onSelect(index)}
                onClick={() => onPaste(emojiForTone(item, tone))}
                className={[
                  "grid aspect-square place-items-center rounded-xl text-[25px] transition-[background-color,transform] duration-150 active:scale-[0.88]",
                  index === selected
                    ? "bg-[oklch(0.62_0.16_255/14%)]"
                    : "hover:bg-black/[0.05] dark:hover:bg-white/[0.08]",
                ].join(" ")}
              >
                {emojiForTone(item, tone)}
              </button>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
