"use client";

import { useEffect, useId, useRef, useState, type ReactNode } from "react";
import { cn } from "@/lib/utils";

/**
 * A (?) that explains without taking room on the page. Opens on hover, on
 * keyboard focus and on click (so touch works), closes on Escape or when the
 * pointer and focus both leave. The panel can hold links, which a native
 * tooltip cannot.
 */
export function HelpTip({ label, children, align = "left", className }: {
  /** What the (?) explains, read by screen readers: "How fairshare decides". */
  label: string;
  children: ReactNode;
  align?: "left" | "right";
  className?: string;
}) {
  const [open, setOpen] = useState(false);
  const [pinned, setPinned] = useState(false);
  const id = useId();
  const wrap = useRef<HTMLSpanElement>(null);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => { if (e.key === "Escape") { setOpen(false); setPinned(false); } };
    const onDown = (e: MouseEvent) => { if (!wrap.current?.contains(e.target as Node)) { setOpen(false); setPinned(false); } };
    document.addEventListener("keydown", onKey);
    document.addEventListener("mousedown", onDown);
    return () => { document.removeEventListener("keydown", onKey); document.removeEventListener("mousedown", onDown); };
  }, [open]);

  const show = () => { if (timer.current) clearTimeout(timer.current); setOpen(true); };
  const hide = () => {
    if (pinned) return;
    if (timer.current) clearTimeout(timer.current);
    timer.current = setTimeout(() => setOpen(false), 120);
  };

  return (
    <span
      ref={wrap}
      className={cn("relative inline-flex", className)}
      onMouseEnter={show}
      onMouseLeave={hide}
      onFocus={show}
      onBlur={(e) => { if (!wrap.current?.contains(e.relatedTarget as Node)) hide(); }}
    >
      <button
        type="button"
        aria-label={label}
        aria-expanded={open}
        aria-controls={id}
        onClick={() => { const close = open && pinned; setOpen(!close); setPinned(!close); }}
        className={cn(
          "inline-flex h-5 w-5 shrink-0 items-center justify-center rounded-full border text-[11.5px] font-semibold transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
          open ? "border-foreground text-foreground" : "border-border text-muted-foreground hover:text-foreground",
        )}
      >
        ?
      </button>
      {open && (
        <span
          id={id}
          role="tooltip"
          className={cn(
            "absolute top-7 z-40 block w-[min(340px,calc(100vw-2rem))] space-y-1.5 rounded-[10px] border border-border bg-[hsl(240_5%_10%)] px-3.5 py-3 text-left text-[12.5px] font-normal leading-relaxed text-secondary-foreground shadow-2xl",
            align === "left" ? "left-0" : "right-0",
          )}
        >
          {children}
        </span>
      )}
    </span>
  );
}
