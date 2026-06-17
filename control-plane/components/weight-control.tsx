"use client";

import { useEffect, useState, useTransition } from "react";
import { RefreshCw, Save } from "lucide-react";
import { setWeightAction } from "@/app/actions";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";

export function WeightControl({
  id,
  initial,
  peerWeightTotal = 0,
  tenantCount = 1,
  onSaved,
}: {
  id: string;
  initial: number;
  peerWeightTotal?: number;
  tenantCount?: number;
  onSaved?: () => void;
}) {
  const [value, setValue] = useState(String(initial));
  const [pending, start] = useTransition();
  const next = Math.max(1, Math.round(Number(value) || 1));
  const changed = next !== initial;
  useEffect(() => {
    setValue(String(initial));
  }, [initial]);

  return (
    <div className="space-y-3">
      <div className="grid min-w-0 grid-cols-[minmax(5rem,1fr)_5rem_auto] items-center gap-2">
        <input
          type="range"
          min={1}
          max={1000}
          value={next}
          onChange={(e) => setValue(e.target.value)}
          aria-label="Fairshare weight"
          className="h-1.5 min-w-0 cursor-pointer appearance-none rounded-full bg-muted accent-foreground"
        />
        <Input
          type="number"
          min={1}
          aria-label="Fairshare weight"
          className="h-8 min-w-0 text-xs"
          value={value}
          onChange={(e) => setValue(e.target.value)}
        />
        <Button
          size="sm"
          variant="secondary"
          disabled={pending || !changed}
          onClick={() =>
            start(async () => {
              await setWeightAction(id, next);
              onSaved?.();
            })
          }
        >
          {pending ? (
            <RefreshCw className="h-3.5 w-3.5 animate-spin" aria-hidden />
          ) : (
            <Save className="h-3.5 w-3.5" aria-hidden />
          )}
          Apply
        </Button>
      </div>
      <WeightImpactMeter
        weight={next}
        peerWeightTotal={peerWeightTotal}
        tenantCount={tenantCount}
      />
    </div>
  );
}

function WeightImpactMeter({
  weight,
  peerWeightTotal,
  tenantCount,
}: {
  weight: number;
  peerWeightTotal: number;
  tenantCount: number;
}) {
  const total = Math.max(1, peerWeightTotal + weight);
  const share = weight / total;
  const average = tenantCount > 0 ? 1 / tenantCount : 1;
  const sharePct = Math.max(2, Math.min(100, share * 100));
  const averagePct = Math.max(0, Math.min(100, average * 100));

  return (
    <div className="rounded-md border border-border/60 bg-background/30 p-3">
      <div className="flex flex-wrap items-baseline justify-between gap-2">
        <p className="text-xs font-medium">Estimated share</p>
        <p className="text-sm font-semibold tabular-nums">{formatPercent(share)}</p>
      </div>
      <div className="relative mt-2 h-2 rounded-sm bg-muted">
        <div className="h-full rounded-sm bg-primary/70" style={{ width: `${sharePct}%` }} />
        <span
          aria-hidden
          className="absolute top-1/2 h-4 w-px -translate-y-1/2 bg-foreground/60"
          style={{ left: `${averagePct}%` }}
        />
      </div>
      <div className="mt-2 flex flex-wrap justify-between gap-2 text-[11px] text-muted-foreground">
        <span>Current weight {weight}</span>
        <span>Average {formatPercent(average)}</span>
      </div>
    </div>
  );
}

function formatPercent(value: number): string {
  if (!Number.isFinite(value)) return "0%";
  const pct = value * 100;
  return pct >= 10 ? `${pct.toFixed(0)}%` : `${pct.toFixed(1)}%`;
}
