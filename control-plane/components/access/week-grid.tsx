"use client";

import { useEffect, useRef, useState } from "react";
import { GRID_SLOTS, gridToWindows, windowsFitGrid, windowsSummary, windowsToGrid } from "@/lib/access-model";
import type { WeeklyWindow } from "@/lib/obleth";
import { cn } from "@/lib/utils";

// Monday first, as a week reads; the gateway numbers Sunday 0.
const ROWS = [
  { day: 1, name: "Mon" },
  { day: 2, name: "Tue" },
  { day: 3, name: "Wed" },
  { day: 4, name: "Thu" },
  { day: 5, name: "Fri" },
  { day: 6, name: "Sat" },
  { day: 0, name: "Sun" },
];

function preset(days: number[], from: number, to: number): boolean[][] {
  const grid = windowsToGrid([]);
  for (const d of days) for (let s = from * 2; s < to * 2; s++) grid[d][s] = true;
  return grid;
}

const PRESETS: { label: string; grid: () => boolean[][] }[] = [
  { label: "Always open", grid: () => windowsToGrid([]) },
  { label: "Weekdays 9–17", grid: () => preset([1, 2, 3, 4, 5], 9, 17) },
  { label: "Weekdays 7–22", grid: () => preset([1, 2, 3, 4, 5], 7, 22) },
  { label: "Every day 7–22", grid: () => preset([0, 1, 2, 3, 4, 5, 6], 7, 22) },
];

/**
 * The week as half-hour cells: press and drag to open or close hours. It
 * submits `weekly_windows` as the gateway's JSON; untouched, it submits the
 * stored windows exactly, so opening the page never reads as a change.
 */
export function WeekGrid({ name, windows }: { name: string; windows: WeeklyWindow[] | null }) {
  const original = useRef(windowsToGrid(windows));
  const [grid, setGrid] = useState(original.current);
  const [touched, setTouched] = useState(false);
  const paint = useRef<boolean | null>(null);

  useEffect(() => {
    const stop = () => { paint.current = null; };
    window.addEventListener("pointerup", stop);
    return () => window.removeEventListener("pointerup", stop);
  }, []);

  const set = (day: number, slot: number, on: boolean) => {
    setGrid((g) => {
      if (g[day][slot] === on) return g;
      const next = g.map((row) => [...row]);
      next[day][slot] = on;
      return next;
    });
    setTouched(true);
  };

  const current = touched ? gridToWindows(grid) : windows ?? [];
  const value = JSON.stringify(current);

  return (
    <div className="flex flex-col gap-1.5">
      <input type="hidden" name={name} value={value} />
      <div className="flex flex-wrap gap-1.5 pb-1">
        {PRESETS.map((p) => (
          <button key={p.label} type="button" onClick={() => { setGrid(p.grid()); setTouched(true); }} className="inline-flex h-7 items-center rounded-full border border-border px-2.5 text-[12px] text-secondary-foreground hover:border-muted-foreground hover:text-foreground">
            {p.label}
          </button>
        ))}
      </div>
      <div className="overflow-x-auto">
        <div className="min-w-[560px] select-none" onPointerLeave={() => { paint.current = null; }}>
          <div className="grid grid-cols-[36px_repeat(24,minmax(0,1fr))] gap-[2px] pb-0.5 font-mono text-[10px] text-muted-foreground">
            <span />
            {Array.from({ length: 24 }, (_, h) => <span key={h} className="text-left">{h % 3 === 0 ? h : ""}</span>)}
          </div>
          {ROWS.map((row) => (
            <div key={row.day} className="grid grid-cols-[36px_repeat(48,minmax(0,1fr))] items-center gap-x-[1px] gap-y-0 py-[1px]">
              <span className="text-[11.5px] text-secondary-foreground">{row.name}</span>
              {Array.from({ length: GRID_SLOTS }, (_, s) => {
                const on = grid[row.day][s];
                const was = original.current[row.day][s];
                return (
                  <span
                    key={s}
                    aria-hidden="true"
                    title={`${row.name} ${Math.floor(s / 2)}:${s % 2 ? "30" : "00"}`}
                    onPointerDown={(e) => { e.preventDefault(); paint.current = !on; set(row.day, s, !on); }}
                    onPointerEnter={() => { if (paint.current !== null) set(row.day, s, paint.current); }}
                    className={cn(
                      "h-[18px] cursor-pointer",
                      s % 2 === 0 ? "rounded-l-[2px]" : "mr-[1px] rounded-r-[2px]",
                      on ? (was ? "bg-muted-foreground" : "bg-foreground") : was ? "bg-muted ring-1 ring-inset ring-muted-foreground/60" : "bg-muted",
                    )}
                  />
                );
              })}
            </div>
          ))}
        </div>
      </div>
      <div className="flex flex-wrap items-center gap-x-4 gap-y-1 pt-1 text-xs text-secondary-foreground">
        <span className="inline-flex items-center gap-1.5"><span className="h-2.5 w-3.5 rounded-[2px] bg-muted-foreground" />open</span>
        {touched && <span className="inline-flex items-center gap-1.5"><span className="h-2.5 w-3.5 rounded-[2px] bg-foreground" />you just opened</span>}
        <span aria-live="polite">{windowsSummary(current)}</span>
      </div>
      {!windowsFitGrid(windows) && !touched && (
        <p className="text-[11.5px] text-muted-foreground">Some hours don&apos;t start or end on the half hour; editing the grid rounds them to it.</p>
      )}
    </div>
  );
}
