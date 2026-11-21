"use client";

import { glyphOf, STATE_LABEL, type DeployState } from "@/lib/deployments-model";
import { AttentionPill, StatusDot, StatusPill } from "@/components/ui/status";

/** A deployment's status as a pill; "needs you" states are the one inverted mark. */
export function StateMark({ state, className }: { state: DeployState; className?: string }) {
  const glyph = glyphOf(state);
  return glyph === "attention"
    ? <AttentionPill className={className}>{STATE_LABEL[state]}</AttentionPill>
    : <StatusPill glyph={glyph} className={className}>{STATE_LABEL[state]}</StatusPill>;
}

/** Up to eight dots for replicas: serving filled, starting half, the rest dashed. */
export function ReplicaDots({ ready, starting = 0, wanted }: { ready: number; starting?: number; wanted: number | null }) {
  const total = Math.max(wanted ?? ready, ready + starting);
  const shown = Math.min(total, 8);
  return (
    <span className="flex items-center gap-[3px]" aria-hidden="true">
      {Array.from({ length: shown }, (_, i) => (
        <StatusDot key={i} glyph={i < ready ? "on" : i < ready + starting ? "half" : "off"} />
      ))}
    </span>
  );
}
