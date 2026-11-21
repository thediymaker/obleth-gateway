"use client";

import type { ReactNode } from "react";
import { SearchBox } from "@/components/ui/search-box";
import { Select, type SelectOption } from "@/components/ui/select";
import { cn } from "@/lib/utils";

/**
 * A list's filters on one row: its search ("/" focuses it, Escape and the
 * clear button empty it), then its pickers and toggles, then anything that
 * belongs at the far end.
 */
export function FilterBar({ search, children, end, className }: {
  search: { value: string; onChange: (value: string) => void; label: string; placeholder?: string; className?: string };
  children?: ReactNode;
  end?: ReactNode;
  className?: string;
}) {
  return (
    <div className={cn("flex flex-wrap items-center gap-2", className)}>
      <SearchBox slash {...search} className={cn("max-w-[380px]", search.className)} />
      {children}
      {end && <div className="ml-auto flex items-center gap-2">{end}</div>}
    </div>
  );
}

/**
 * The filter row's one dropdown style. With `unset` (the value that filters
 * nothing), it is outlined while it filters something else.
 */
export function FilterSelect<T extends string>({ label, value, onChange, options, unset }: {
  label: string;
  value: T;
  onChange: (value: T) => void;
  options: readonly (SelectOption & { value: T })[];
  unset?: T;
}) {
  return (
    <Select
      aria-label={label}
      value={value}
      onValueChange={(v) => onChange(v as T)}
      options={options}
      className={cn("h-9 w-auto min-w-[10rem] text-[12.5px]", unset !== undefined && value !== unset && "border-foreground text-foreground")}
    />
  );
}
