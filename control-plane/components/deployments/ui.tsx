"use client";

import { AlertCircle } from "lucide-react";
import { glyphOf, STATE_LABEL, type DeployState, type Glyph, type ReplicaPhase } from "@/lib/deployments-model";
import { cn } from "@/lib/utils";

/** Filled dot serving, half dot starting or queued, dashed ring not running. */
export function Glyph({ glyph, className }: { glyph: Glyph | ReplicaPhase; className?: string }) {
  const g = glyph === "serving" ? "on" : glyph === "queued" || glyph === "running" ? "half" : glyph === "draining" || glyph === "lost" ? "off" : glyph;
  return (
    <span
      aria-hidden="true"
      className={cn(
        "inline-block h-2 w-2 shrink-0 rounded-full",
        g === "on" && "bg-secondary-foreground",
        g === "half" && "border-[1.5px] border-secondary-foreground bg-[linear-gradient(90deg,hsl(240_5%_84%)_50%,transparent_50%)]",
        (g === "off" || g === "attention") && "border-[1.5px] border-dashed border-muted-foreground",
        className,
      )}
    />
  );
}

/** A deployment's status as a pill; "needs you" states are the one inverted mark. */
export function StateMark({ state, className }: { state: DeployState; className?: string }) {
  const glyph = glyphOf(state);
  if (glyph === "attention") {
    return (
      <span className={cn("inline-flex h-[22px] items-center gap-1 whitespace-nowrap rounded-full bg-foreground px-2 text-[11.5px] font-semibold text-background", className)}>
        <AlertCircle className="h-3 w-3" aria-hidden />{STATE_LABEL[state]}
      </span>
    );
  }
  return (
    <span className={cn("inline-flex h-[22px] items-center gap-1.5 whitespace-nowrap rounded-full border border-border px-2 text-[11.5px] font-medium text-secondary-foreground", className)}>
      <Glyph glyph={glyph} className="h-[7px] w-[7px]" />{STATE_LABEL[state]}
    </span>
  );
}

/** Up to eight dots for replicas: serving filled, starting half, the rest dashed. */
export function ReplicaDots({ ready, starting = 0, wanted }: { ready: number; starting?: number; wanted: number | null }) {
  const total = Math.max(wanted ?? ready, ready + starting);
  const shown = Math.min(total, 8);
  return (
    <span className="flex items-center gap-[3px]" aria-hidden="true">
      {Array.from({ length: shown }, (_, i) => (
        <Glyph key={i} glyph={i < ready ? "on" : i < ready + starting ? "half" : "off"} />
      ))}
    </span>
  );
}
