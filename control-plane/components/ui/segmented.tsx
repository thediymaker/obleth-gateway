"use client";

import type { ReactNode } from "react";
import { cn } from "@/lib/utils";

/** A row of mutually exclusive buttons. `stretch` makes them share the row's width. */
export function Segmented<T extends string>({ value, options, onChange, label, stretch, className }: {
  value: T; options: { value: T; label: ReactNode }[]; onChange: (value: T) => void; label: string; stretch?: boolean; className?: string;
}) {
  return (
    <div role="group" aria-label={label} className={cn("gap-0.5 rounded-[9px] border border-border p-[3px]", stretch ? "flex" : "inline-flex flex-wrap", className)}>
      {options.map((o) => (
        <button
          key={o.value}
          type="button"
          aria-pressed={value === o.value}
          onClick={() => onChange(o.value)}
          className={cn(
            "inline-flex h-7 items-center justify-center whitespace-nowrap rounded-md px-2.5 text-[12.5px] transition-colors focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring",
            stretch && "flex-1",
            value === o.value ? "bg-secondary text-foreground" : "text-muted-foreground hover:text-foreground",
          )}
        >
          {o.label}
        </button>
      ))}
    </div>
  );
}
