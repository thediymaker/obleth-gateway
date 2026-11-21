"use client";

import { cn } from "@/lib/utils";
import { type StatusGlyph } from "@/components/ui/status";

export const TENANT_STATUS_LABEL: Record<string, string> = { active: "Active", suspended: "Suspended", archived: "Archived" };

/** Active is on; suspended and archived are both deliberately off. */
export function tenantGlyph(status: string): StatusGlyph {
  return status === "active" ? "on" : "off";
}

/** A thin bar for how much of a budget is used; bright once it's near. */
export function BudgetBar({ share, className }: { share: number | null; className?: string }) {
  const pct = share == null ? 0 : Math.min(100, Math.max(0, share * 100));
  return (
    <span aria-hidden="true" className={cn("block h-1.5 overflow-hidden rounded-full bg-muted", share == null && "opacity-40", className)}>
      <span className={cn("block h-full rounded-full", (share ?? 0) >= 0.8 ? "bg-foreground" : "bg-secondary-foreground")} style={{ width: `${pct}%` }} />
    </span>
  );
}
