"use client";

import Link from "next/link";
import { useEffect, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { cn, formatCurrency, formatNumber } from "@/lib/utils";
import type { OverviewSummary } from "@/lib/overview-summary";

const POLL_MS = 30_000;

function Divider() {
  return <div className="hidden h-3.5 w-px shrink-0 bg-border sm:block" aria-hidden />;
}

function Metric({
  label,
  value,
  detail,
  className,
}: {
  label: string;
  value: string;
  detail?: string;
  className?: string;
}) {
  return (
    <div className={cn("flex shrink-0 items-baseline gap-1.5", className)}>
      <span className="text-[10px] uppercase tracking-wider text-muted-foreground">{label}</span>
      <span className="text-xs font-medium tabular-nums">{value}</span>
      {detail && <span className="hidden text-[10px] tabular-nums text-muted-foreground/70 lg:inline">{detail}</span>}
    </div>
  );
}

function FooterSkeleton() {
  return (
    <footer className="flex h-10 shrink-0 items-center gap-4 border-t border-border bg-card/30 px-4 sm:px-6">
      <span className="shrink-0 text-[10px] font-medium uppercase tracking-wider text-muted-foreground">24h</span>
      <div className="flex flex-1 items-center gap-4">
        {Array.from({ length: 6 }).map((_, i) => (
          <div key={i} className="h-3 w-16 animate-pulse rounded bg-muted/60" />
        ))}
      </div>
    </footer>
  );
}

export function StatusFooter() {
  const [mounted, setMounted] = useState(false);
  const { data, isLoading, isError, isFetching } = useQuery({
    queryKey: ["live-summary"],
    queryFn: async () => {
      const res = await fetch("/api/live/summary");
      if (!res.ok) throw new Error("summary fetch failed");
      return (await res.json()) as OverviewSummary;
    },
    enabled: mounted,
    refetchInterval: POLL_MS,
    staleTime: POLL_MS / 2,
  });

  useEffect(() => {
    setMounted(true);
  }, []);

  if (!mounted || (isLoading && !data)) return <FooterSkeleton />;

  const summary = data;
  const avgTokens = summary && summary.requests > 0 ? Math.round(summary.tokens / summary.requests) : 0;

  return (
    <footer className="flex h-10 shrink-0 items-center gap-3 overflow-x-auto border-t border-border bg-card/30 px-4 sm:gap-4 sm:px-6">
      <Link
        href="/"
        className="shrink-0 text-[10px] font-medium uppercase tracking-wider text-muted-foreground transition-colors hover:text-foreground"
        title="Open overview"
      >
        24h
      </Link>

      {!summary ? (
        <span className="text-xs text-muted-foreground">Metrics unavailable</span>
      ) : (
        <div className="flex min-w-0 flex-1 items-center gap-3 sm:gap-4">
          <Metric label="Req" value={formatNumber(summary.requests)} />
          <Divider />
          <Metric
            label="Tok"
            value={formatNumber(summary.tokens)}
            detail={avgTokens > 0 ? `~${formatNumber(avgTokens)}/req` : undefined}
          />
          <Divider />
          <Metric
            label="Cost"
            value={summary.hasPricing ? formatCurrency(summary.cost) : "--"}
            detail={summary.hasPricing ? undefined : "no pricing"}
          />
          <Divider />
          <Metric
            label="Tenants"
            value={formatNumber(summary.tenantCount)}
            detail={`${formatNumber(summary.activeTenants)} active`}
          />
          <Divider />
          <Metric
            label="Models"
            value={formatNumber(summary.enabledModels)}
            detail={`${summary.modelCount} total`}
            className="hidden md:flex"
          />
          <Divider />
          <Metric label="Keys" value={formatNumber(summary.keyCount)} className="hidden md:flex" />
        </div>
      )}

      <div
        className={cn(
          "ml-auto shrink-0 text-[10px] tabular-nums text-muted-foreground/60",
          isError && "text-destructive/80",
        )}
        title={isError ? "Last refresh failed; showing cached values" : undefined}
      >
        {isFetching ? "..." : ""}
      </div>
    </footer>
  );
}
