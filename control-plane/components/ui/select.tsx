"use client";

import * as React from "react";
import { Check, ChevronDown, Search } from "lucide-react";
import { cn } from "@/lib/utils";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";

/** Lists at least this long get a filter box in the popup. */
const SEARCH_THRESHOLD = 10;

export type SelectOption = {
  value: string;
  label: string;
  /** Secondary text shown under the label in the popup. */
  hint?: string;
};

export interface SelectProps {
  options: readonly SelectOption[];
  /** Controlled value. Pair with onValueChange. */
  value?: string;
  /** Uncontrolled starting value. */
  defaultValue?: string;
  onValueChange?: (value: string) => void;
  /** Set to submit the value with a surrounding form. */
  name?: string;
  id?: string;
  disabled?: boolean;
  required?: boolean;
  /** Draws the destructive border, for a value that fails validation. */
  invalid?: boolean;
  /** Shown when the value matches no option. */
  placeholder?: string;
  /** Classes for the trigger. */
  className?: string;
  /** Classes for the popup. */
  contentClassName?: string;
  align?: "start" | "center" | "end";
  /** Overrides the automatic filter box. */
  searchable?: boolean;
  searchPlaceholder?: string;
  "aria-label"?: string;
  "aria-labelledby"?: string;
}

/**
 * Dropdown that matches the portal's own surfaces instead of the browser's
 * native select popup. Keyboard: arrows and Enter move and pick, printable
 * keys type into the filter box when there is one.
 */
export function Select({
  options,
  value,
  defaultValue,
  onValueChange,
  name,
  id,
  disabled,
  required,
  invalid,
  placeholder = "Select an option",
  className,
  contentClassName,
  align = "start",
  searchable,
  searchPlaceholder = "Filter options",
  ...aria
}: SelectProps) {
  const [uncontrolled, setUncontrolled] = React.useState(defaultValue ?? "");
  const current = value !== undefined ? value : uncontrolled;

  const [open, setOpen] = React.useState(false);
  const [query, setQuery] = React.useState("");
  const inputRef = React.useRef<HTMLInputElement>(null);
  const contentRef = React.useRef<HTMLDivElement>(null);

  const showSearch = searchable ?? options.length >= SEARCH_THRESHOLD;
  const selected = options.find((option) => option.value === current);

  const needle = query.trim().toLowerCase();
  const matches = needle
    ? options.filter((option) => `${option.label} ${option.hint ?? ""}`.toLowerCase().includes(needle))
    : options;

  // The menu focuses its first option on open; the filter box wants that focus instead.
  React.useEffect(() => {
    if (!open || !showSearch) return;
    const frame = requestAnimationFrame(() => inputRef.current?.focus());
    return () => cancelAnimationFrame(frame);
  }, [open, showSearch]);

  function pick(next: string) {
    if (value === undefined) setUncontrolled(next);
    onValueChange?.(next);
  }

  function focusFirstOption() {
    contentRef.current?.querySelector<HTMLElement>("[role='menuitem']")?.focus();
  }

  function onSearchKeyDown(event: React.KeyboardEvent) {
    const typing = event.key.length === 1 && !event.metaKey && !event.ctrlKey && !event.altKey;
    // The menu's own typeahead would grab these and move focus off the filter box.
    if (typing || event.key === "Backspace") {
      event.stopPropagation();
      return;
    }
    if (event.key === "ArrowDown") {
      event.preventDefault();
      focusFirstOption();
      return;
    }
    // Enter takes the top match only while filtering; with an empty box it just closes.
    if (event.key === "Enter") {
      event.preventDefault();
      if (needle && matches.length > 0) pick(matches[0].value);
      setOpen(false);
    }
  }

  /** Keeps typing in the filter box even after the arrow keys move focus onto an option. */
  function routeTypingToSearch(event: React.KeyboardEvent) {
    if (!showSearch || event.target === inputRef.current) return;
    if (event.metaKey || event.ctrlKey || event.altKey) return;
    if (event.key === "Backspace") {
      event.preventDefault();
      setQuery((q) => q.slice(0, -1));
      inputRef.current?.focus();
      return;
    }
    if (event.key.length !== 1) return;
    event.preventDefault();
    setQuery((q) => q + event.key);
    inputRef.current?.focus();
  }

  return (
    <>
      {name !== undefined && (
        <select
          name={name}
          value={current}
          required={required}
          disabled={disabled}
          aria-hidden
          tabIndex={-1}
          onChange={() => {}}
          className="pointer-events-none absolute h-0 w-0 border-0 p-0 opacity-0"
        >
          {current !== "" && !selected && <option value={current} />}
          {options.map((option) => (
            <option key={option.value} value={option.value} />
          ))}
        </select>
      )}
      <DropdownMenu
        open={open}
        onOpenChange={(next) => {
          setOpen(next);
          if (!next) setQuery("");
        }}
      >
        <DropdownMenuTrigger
          id={id}
          disabled={disabled}
          aria-invalid={invalid || undefined}
          {...aria}
          className={cn(
            "flex h-9 w-full items-center justify-between gap-2 rounded-md border bg-background px-3 text-sm shadow-sm transition-colors",
            "hover:border-muted-foreground/50 focus:outline-none focus-visible:ring-1 focus-visible:ring-ring",
            "data-[state=open]:ring-1 data-[state=open]:ring-ring disabled:cursor-not-allowed disabled:opacity-50",
            invalid ? "border-destructive" : "border-input",
            className,
          )}
        >
          <span className={cn("truncate", !selected && "text-muted-foreground")}>{selected?.label ?? placeholder}</span>
          <ChevronDown className="h-4 w-4 shrink-0 text-muted-foreground" />
        </DropdownMenuTrigger>
        <DropdownMenuContent
          ref={contentRef}
          align={align}
          sideOffset={6}
          collisionPadding={12}
          onKeyDown={routeTypingToSearch}
          className={cn("w-[var(--radix-dropdown-menu-trigger-width)] min-w-[10rem] p-0 shadow-xl", contentClassName)}
        >
          {showSearch && (
            <div className="flex items-center gap-2 border-b border-border px-3">
              <Search className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
              <input
                ref={inputRef}
                value={query}
                onChange={(event) => setQuery(event.target.value)}
                onKeyDown={onSearchKeyDown}
                placeholder={searchPlaceholder}
                aria-label={searchPlaceholder}
                className="h-9 w-full bg-transparent text-sm outline-none placeholder:text-muted-foreground"
              />
            </div>
          )}
          <div className="max-h-64 overflow-y-auto overscroll-contain p-1.5">
            {matches.length === 0 ? (
              <p className="px-3 py-2 text-sm text-muted-foreground">No options match {`"${query.trim()}"`}</p>
            ) : (
              matches.map((option) => (
                <DropdownMenuItem
                  key={option.value}
                  onSelect={() => pick(option.value)}
                  className="cursor-pointer items-start justify-between gap-2 rounded-md py-2 pl-3 pr-3 data-[state=checked]:bg-secondary"
                >
                  <span className="min-w-0">
                    <span className="block truncate">{option.label}</span>
                    {option.hint && <span className="mt-0.5 block truncate text-xs text-muted-foreground">{option.hint}</span>}
                  </span>
                  {option.value === current && (
                    <Check className="mt-0.5 h-3.5 w-3.5 shrink-0 text-primary" strokeWidth={2.5} />
                  )}
                </DropdownMenuItem>
              ))
            )}
          </div>
        </DropdownMenuContent>
      </DropdownMenu>
    </>
  );
}
