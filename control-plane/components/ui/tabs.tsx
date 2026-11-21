"use client";

import { useRef, type KeyboardEvent } from "react";
import { cn } from "@/lib/utils";

export interface Tab<T extends string> {
  value: T;
  label: string;
  /** A count after the label: "Saved · 3". */
  count?: number;
}

/**
 * A page's tab strip: underlined, one tab stop, and the arrow keys, Home and
 * End move between tabs.
 */
export function Tabs<T extends string>({ label, value, onChange, tabs, className }: {
  label: string;
  value: T;
  onChange: (value: T) => void;
  tabs: readonly Tab<T>[];
  className?: string;
}) {
  const strip = useRef<HTMLDivElement>(null);
  function onKeyDown(e: KeyboardEvent) {
    const i = tabs.findIndex((t) => t.value === value);
    const next = e.key === "ArrowRight" ? (i + 1) % tabs.length : e.key === "ArrowLeft" ? (i - 1 + tabs.length) % tabs.length : e.key === "Home" ? 0 : e.key === "End" ? tabs.length - 1 : -1;
    if (next < 0) return;
    e.preventDefault();
    onChange(tabs[next].value);
    strip.current?.querySelectorAll<HTMLButtonElement>("[role=tab]")[next]?.focus();
  }
  return (
    <div ref={strip} role="tablist" aria-label={label} onKeyDown={onKeyDown} className={cn("flex gap-5 border-b border-border", className)}>
      {tabs.map((t) => (
        <button
          key={t.value}
          type="button"
          role="tab"
          aria-selected={t.value === value}
          tabIndex={t.value === value ? 0 : -1}
          onClick={() => onChange(t.value)}
          className={cn(
            "-mb-px h-10 border-b-2 px-0.5 text-[13.5px] focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
            t.value === value ? "border-foreground font-medium text-foreground" : "border-transparent text-muted-foreground hover:text-foreground",
          )}
        >
          {t.count == null ? t.label : `${t.label} · ${t.count}`}
        </button>
      ))}
    </div>
  );
}
