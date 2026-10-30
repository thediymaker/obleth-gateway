"use client";

import { useEffect, useState } from "react";
import Link from "next/link";
import { keepPreviousData, useQuery } from "@tanstack/react-query";
import { ArrowRight, RefreshCw } from "lucide-react";
import { Pill, Segmented } from "@/components/overview/ui";
import { classLabel, ERROR_CLASSES, isErrorClass, SIDE_LABEL } from "@/lib/failure-classes";
import {
  FAILURE_RANGES,
  failureHeadline,
  groupLogsHref,
  groupsFor,
  hasKey,
  rangeOf,
  seenAgo,
  share,
  type FailureRange,
  type FailureSide,
} from "@/lib/failures-model";
import { logsHref } from "@/lib/log-links";
import { HTTP_REASONS } from "@/lib/logs-model";
import type { UsageFailureGroup, UsageFailureGroups } from "@/lib/obleth";
import { cn, getJson } from "@/lib/utils";

const POLL_MS: Record<FailureRange, number> = { "1h": 15_000, "24h": 30_000, "7d": 120_000 };

function GroupRow({ g, range, now }: { g: UsageFailureGroup; range: FailureRange; now: number }) {
  const href = groupLogsHref(g, range);
  const hint = isErrorClass(g.error_class) ? ERROR_CLASSES[g.error_class].hint : undefined;
  const reason = g.reason.trim() || `HTTP ${g.status_code}`;
  return (
    <tr className="border-t border-border/70 align-top transition-colors hover:bg-muted/30">
      <td className="whitespace-nowrap py-2.5 pl-[18px] pr-3 text-right">
        <Link href={href} className="font-mono text-[13px] font-semibold tabular-nums underline-offset-[3px] hover:underline">{g.requests.toLocaleString()}</Link>
      </td>
      <td className="py-2.5 pr-3">
        <Pill inverted={g.platform} className="h-5 text-[11px]">{SIDE_LABEL[g.platform ? "platform" : "client"]}</Pill>
      </td>
      <td className="max-w-[24rem] py-2.5 pr-3">
        <span className={cn("block", g.platform && "font-medium")} title={hint}>{classLabel(g.error_class) || "Not classified"}</span>
        <span className="mt-0.5 block break-words font-mono text-[11.5px] text-muted-foreground" title={g.reason || undefined}>{reason.length > 160 ? `${reason.slice(0, 160)}…` : reason}</span>
      </td>
      <td className="max-w-[14rem] py-2.5 pr-3">
        <span className="block truncate">{hasKey(g) ? g.key_name || `${g.key_id.slice(0, 8)}…` : "No key"}</span>
        <span className="block truncate text-[11.5px] text-muted-foreground">{g.tenant_name || (hasKey(g) ? `${g.tenant_id.slice(0, 8)}…` : "—")}</span>
      </td>
      <td className="max-w-[14rem] py-2.5 pr-3"><span className="block truncate font-mono text-[12.5px]">{g.model || "—"}</span></td>
      <td className="whitespace-nowrap py-2.5 pr-3">
        <span className="font-mono text-[12.5px]">{g.status_code}</span>
        {HTTP_REASONS[g.status_code] && <span className="block text-[11.5px] text-muted-foreground">{HTTP_REASONS[g.status_code]}</span>}
      </td>
      <td className="whitespace-nowrap py-2.5 pr-3 text-[12.5px] text-secondary-foreground" title={new Date(g.first_ms).toLocaleString()}>{seenAgo(g.first_ms, now)}</td>
      <td className="whitespace-nowrap py-2.5 pr-3 text-[12.5px] text-secondary-foreground" title={new Date(g.last_ms).toLocaleString()}>{seenAgo(g.last_ms, now)}</td>
      <td className="whitespace-nowrap py-2.5 pr-[18px] text-right">
        <Link href={href} aria-label={`Requests: ${classLabel(g.error_class)} on ${g.model}`} className="inline-flex items-center gap-1 text-[12.5px] text-secondary-foreground hover:text-foreground">
          Requests<ArrowRight className="h-3.5 w-3.5" aria-hidden="true" />
        </Link>
      </td>
    </tr>
  );
}

/**
 * Which requests failed and whose side each failure was on: the platform's
 * (it could not serve a valid request) or the caller's (the request could
 * not be served as sent). The same failure repeated shows as one row.
 */
export function FailuresView({ initialRange, initial }: { initialRange: FailureRange; initial: UsageFailureGroups | null }) {
  const [range, setRange] = useState<FailureRange>(initialRange);
  const [side, setSide] = useState<FailureSide>("all");
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const t = setInterval(() => setNow(Date.now()), 15_000);
    return () => clearInterval(t);
  }, []);

  const query = useQuery({
    queryKey: ["usage-failures", range],
    queryFn: () => getJson<UsageFailureGroups>(`/api/live/usage/failures?since_ms=${Date.now() - rangeOf(range).ms}`),
    initialData: range === initialRange && initial ? initial : undefined,
    placeholderData: keepPreviousData,
    refetchInterval: POLL_MS[range],
  });
  const data = query.data;
  const groups = data ? groupsFor(data.groups, side) : [];
  const callers = data ? Math.max(0, data.failures - data.platform_failures) : 0;

  return (
    <div className="mx-auto flex max-w-[1600px] flex-col gap-4">
      <div className="flex flex-wrap items-end justify-between gap-4">
        <div className="min-w-0">
          <h1 className="text-[26px] font-semibold tracking-tight">Failures</h1>
          <p className="mt-1 text-[13px] text-muted-foreground" role="status">
            {data ? `${failureHeadline(data, range)}.` : query.isLoading ? "Counting…" : "The failures could not be read."}
          </p>
        </div>
        <div className="flex items-center gap-2.5">
          <Segmented label="Time range" value={range} onChange={setRange} options={FAILURE_RANGES.map((r) => ({ value: r.id, label: r.label }))} />
          <button type="button" onClick={() => void query.refetch()} aria-label="Refresh" title="Refresh" className="flex h-[34px] w-[34px] items-center justify-center rounded-lg border border-border text-muted-foreground hover:bg-accent hover:text-foreground">
            <RefreshCw className={cn("h-3.5 w-3.5", query.isFetching && "animate-spin")} />
          </button>
        </div>
      </div>

      {data && data.failures > 0 && (
        <div className={cn("grid gap-3 sm:grid-cols-2", query.isPlaceholderData && "opacity-60")}>
          <Link href={logsHref({ fault: "platform", window: range })} className="flex flex-col gap-1 rounded-xl border border-border bg-card px-4 py-3 transition-colors hover:border-muted-foreground/60">
            <span className="text-[11px] font-semibold uppercase tracking-[0.07em] text-muted-foreground">On the platform&apos;s side</span>
            <span className="text-[26px] font-semibold leading-tight tracking-tight tabular-nums">{data.platform_failures.toLocaleString()}</span>
            <span className="text-xs text-muted-foreground">{data.platform_failures ? `${share(data.platform_failures, data.requests)} of requests · the gateway or a model server could not serve them` : "The gateway and model servers served every valid request"}</span>
          </Link>
          <Link href={logsHref({ fault: "client", window: range })} className="flex flex-col gap-1 rounded-xl border border-border bg-card px-4 py-3 transition-colors hover:border-muted-foreground/60">
            <span className="text-[11px] font-semibold uppercase tracking-[0.07em] text-muted-foreground">From callers</span>
            <span className="text-[26px] font-semibold leading-tight tracking-tight tabular-nums text-secondary-foreground">{callers.toLocaleString()}</span>
            <span className="text-xs text-muted-foreground">{callers ? `${share(callers, data.requests)} of requests · refused as sent, hung up, or over a limit` : "Every caller's request could be served as sent"}</span>
          </Link>
        </div>
      )}

      <section aria-label="Failures, grouped" className={cn("overflow-hidden rounded-xl border border-border bg-card", query.isPlaceholderData && "opacity-60")}>
        <div className="flex flex-wrap items-start justify-between gap-3 px-[18px] pb-3 pt-4">
          <div className="min-w-0">
            <h2 className="text-sm font-semibold">The same failure, grouped</h2>
            <p className="mt-0.5 text-xs text-muted-foreground">One row per key, model, status, kind of failure and reason. The 50 largest, most first. Click a count to see its requests.</p>
          </div>
          <Segmented
            label="Whose failures"
            value={side}
            onChange={setSide}
            options={[{ value: "all", label: "Both sides" }, { value: "platform", label: "Platform" }, { value: "client", label: "Callers" }]}
          />
        </div>
        {groups.length > 0 ? (
          <div className="overflow-x-auto">
            <table className="w-full min-w-[1040px] text-[13px]">
              <thead>
                <tr className="text-left text-[11px] font-semibold uppercase tracking-[0.07em] text-muted-foreground">
                  <th className="py-2.5 pl-[18px] pr-3 text-right font-semibold">Failed</th>
                  <th className="py-2.5 pr-3 font-semibold">Side</th>
                  <th className="py-2.5 pr-3 font-semibold">Why · reason</th>
                  <th className="py-2.5 pr-3 font-semibold">Key · team</th>
                  <th className="py-2.5 pr-3 font-semibold">Model</th>
                  <th className="py-2.5 pr-3 font-semibold">Status</th>
                  <th className="py-2.5 pr-3 font-semibold">First seen</th>
                  <th className="py-2.5 pr-3 font-semibold">Last seen</th>
                  <th className="py-2.5 pr-[18px]"><span className="sr-only">Requests</span></th>
                </tr>
              </thead>
              <tbody>
                {groups.map((g) => <GroupRow key={`${g.error_class}:${g.key_id}:${g.model}:${g.status_code}:${g.reason}:${g.first_ms}`} g={g} range={range} now={now} />)}
              </tbody>
            </table>
          </div>
        ) : (
          <div className="border-t border-border px-6 py-14 text-center text-[13px] text-muted-foreground">
            {query.isLoading ? <div className="skeleton mx-auto h-4 w-48 rounded" /> : !data ? "The failures could not be read." : side === "all" ? `Nothing failed in ${rangeOf(range).words}.` : `Nothing failed ${side === "platform" ? "on the platform's side" : "from callers"} in ${rangeOf(range).words}.`}
          </div>
        )}
      </section>
    </div>
  );
}
