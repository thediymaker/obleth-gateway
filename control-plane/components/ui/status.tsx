"use client";

import type { ReactNode } from "react";
import { AlertCircle } from "lucide-react";
import { cn } from "@/lib/utils";
import { Pill } from "@/components/ui/pill";

/**
 * The dashboard's one status vocabulary, so a mark means the same on every page:
 *
 * - filled dot: on (serving, answering, active, done)
 * - half dot: in progress (starting, queued, checking, indexing)
 * - ring: no data yet (not checked, not reported)
 * - dashed ring: deliberately not serving (off, paused, suspended, archived, maintenance)
 * - inverted pill: needs someone (failing, can't start, not answering)
 *
 * Status is monochrome everywhere. The dashboard's one colour is the
 * assistant's violet, kept to the assistant's own messages (charo/rail.tsx).
 */
export type StatusGlyph = "on" | "half" | "none" | "off";

/** The legend line under a table that uses these marks. */
export const STATUS_LEGEND = "Filled dot on · half dot in progress · ring no data yet · dashed ring off · white pill needs you";

export function StatusDot({ glyph, label, pulse, className }: { glyph: StatusGlyph; label?: string; pulse?: boolean; className?: string }) {
  return (
    <span
      role={label ? "img" : undefined}
      aria-label={label}
      aria-hidden={label ? undefined : true}
      data-glyph={glyph}
      className={cn(
        "inline-block h-2 w-2 shrink-0 rounded-full",
        glyph === "on" && "bg-secondary-foreground",
        glyph === "half" && "border-[1.5px] border-secondary-foreground bg-[linear-gradient(90deg,hsl(var(--secondary-foreground))_50%,transparent_50%)]",
        glyph === "none" && "border-[1.5px] border-muted-foreground",
        glyph === "off" && "border-[1.5px] border-dashed border-muted-foreground",
        pulse && "animate-pulse",
        className,
      )}
    />
  );
}

/** The one attention mark: a status that needs someone, as an inverted pill. */
export function AttentionPill({ children, icon = true, className }: { children: ReactNode; icon?: boolean; className?: string }) {
  return (
    <span data-glyph="attention" className={cn("inline-flex h-[22px] shrink-0 items-center gap-1 whitespace-nowrap rounded-full bg-foreground px-2 text-[11.5px] font-semibold text-background", className)}>
      {icon && <AlertCircle className="h-3 w-3" aria-hidden />}
      {children}
    </span>
  );
}

/** A status as a pill: its dot and word, or the attention pill when it needs someone. */
export function StatusPill({ glyph, attention, children, className }: { glyph: StatusGlyph; attention?: boolean; children: ReactNode; className?: string }) {
  if (attention) return <AttentionPill className={className}>{children}</AttentionPill>;
  return <Pill className={className}><StatusDot glyph={glyph} className="h-[7px] w-[7px]" />{children}</Pill>;
}

/** A status as a dot and its word, for table cells; the attention pill when it needs someone. */
export function StatusText({ glyph, attention, children, className }: { glyph: StatusGlyph; attention?: boolean; children: ReactNode; className?: string }) {
  if (attention) return <AttentionPill className={cn("h-5 px-2 text-[11px]", className)} icon={false}>{children}</AttentionPill>;
  return (
    <span className={cn("inline-flex items-center gap-2 text-[12.5px]", glyph === "on" ? "text-secondary-foreground" : glyph === "off" ? "text-muted-foreground" : "text-foreground", className)}>
      <StatusDot glyph={glyph} />
      {children}
    </span>
  );
}
