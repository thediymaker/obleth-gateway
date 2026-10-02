"use client";

import { useEffect, useRef, useState } from "react";
import { ChevronDown } from "lucide-react";
import { cn } from "@/lib/utils";

/** "Thought for 4.2 s" / "Thought for 1 min 12 s". */
export function thoughtFor(ms: number): string {
  const s = ms / 1000;
  if (s < 10) return `${s.toFixed(1)} s`;
  if (s < 60) return `${Math.round(s)} s`;
  const whole = Math.round(s);
  return `${Math.floor(whole / 60)} min ${whole % 60} s`;
}

/**
 * A model's thinking, above its answer. Open and following along while the
 * model is still thinking; folded to one line once the answer starts, so the
 * answer reads first. Either way a click toggles it.
 */
export function ThinkingBlock({ text, active, ms }: { text: string; active: boolean; ms?: number }) {
  const [choice, setChoice] = useState<boolean | null>(null);
  const open = choice ?? active;
  const body = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (active && open && body.current) body.current.scrollTop = body.current.scrollHeight;
  }, [text, active, open]);
  const words = text.trim() ? text.trim().split(/\s+/).length : 0;
  const label = active ? "Thinking…" : ms !== undefined ? `Thought for ${thoughtFor(ms)}` : "Thinking";

  return (
    <div className="w-full rounded-lg border border-border bg-muted/30">
      <button
        type="button"
        aria-expanded={open}
        onClick={() => setChoice(!open)}
        className="flex w-full items-center gap-2 px-3 py-1.5 text-left text-[12px] text-muted-foreground hover:text-foreground"
      >
        <span className="font-medium">{label}</span>
        <span className="tabular-nums">{words.toLocaleString()} word{words === 1 ? "" : "s"}</span>
        <ChevronDown className={cn("ml-auto h-3.5 w-3.5 transition-transform", open && "rotate-180")} aria-hidden="true" />
      </button>
      {open && (
        <div
          ref={body}
          aria-label="Model thinking"
          className="max-h-72 overflow-y-auto whitespace-pre-wrap break-words border-t border-border px-3 py-2 text-[12.5px] leading-relaxed text-muted-foreground"
        >
          {text}
        </div>
      )}
    </div>
  );
}
