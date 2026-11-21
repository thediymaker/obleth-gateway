"use client";

import { useEffect, useRef, type KeyboardEvent as ReactKeyboardEvent } from "react";
import { Search, X } from "lucide-react";
import { cn } from "@/lib/utils";

/**
 * A list's search field. With `slash`, "/" anywhere on the page focuses it
 * (not while typing in another field) and it shows the key; Escape and the
 * clear button empty it.
 */
export function SearchBox({ value, onChange, label, placeholder, slash, onKeyDown, className }: {
  value: string;
  onChange: (value: string) => void;
  /** What it searches, for screen readers: "Search models". */
  label: string;
  placeholder?: string;
  slash?: boolean;
  onKeyDown?: (e: ReactKeyboardEvent<HTMLInputElement>) => void;
  className?: string;
}) {
  const input = useRef<HTMLInputElement>(null);
  useEffect(() => {
    if (!slash) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "/" || (e.target instanceof Element && e.target.closest("input, textarea, select, [contenteditable=true]"))) return;
      e.preventDefault();
      input.current?.focus();
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [slash]);
  return (
    <label className={cn("flex h-9 min-w-[240px] flex-1 items-center gap-2 rounded-lg border border-border bg-background px-3 text-[13px] focus-within:border-muted-foreground", className)}>
      <Search className="h-4 w-4 shrink-0 text-muted-foreground" aria-hidden />
      <input
        ref={input}
        value={value}
        onChange={(e) => onChange(e.target.value)}
        onKeyDown={(e) => {
          onKeyDown?.(e);
          if (!e.defaultPrevented && e.key === "Escape" && value) onChange("");
        }}
        placeholder={placeholder}
        aria-label={label}
        className="min-w-0 flex-1 bg-transparent outline-none placeholder:text-muted-foreground"
      />
      {value ? (
        <button type="button" onClick={() => { onChange(""); input.current?.focus(); }} aria-label="Clear search" className="text-muted-foreground hover:text-foreground"><X className="h-3.5 w-3.5" /></button>
      ) : slash ? (
        <kbd className="rounded border border-border px-1.5 font-mono text-[11px] text-muted-foreground">/</kbd>
      ) : null}
    </label>
  );
}
