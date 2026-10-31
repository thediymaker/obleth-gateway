"use client";

import { useEffect, useState } from "react";
import Link from "next/link";
import { keepPreviousData, useQuery } from "@tanstack/react-query";
import { RefreshCw } from "lucide-react";
import { Kpi, Meter, Panel, PanelLink, Segmented } from "@/components/overview/ui";
import { classLabel, ERROR_CLASSES, isErrorClass } from "@/lib/failure-classes";
import {
  FAILURE_RANGES,
  failureHeadline,
  groupLogsHref,
  groupsFor,
  hasKey,
  rangeOf,
  seenAgo,
  trendOf,
  TREND_POINTS,
  type FailureRange,
} from "@/lib/failures-model";
import { logsHref } from "@/lib/log-links";
import { compact } from "@/lib/overview-model";
import type { UsageFailureGroup, UsageFailureGroups, UsageLogHistogram } from "@/lib/obleth";
import { cn, getJson } from "@/lib/utils";

const POLL_MS: Record<FailureRange, number> = { "1h": 15_000, "24h": 30_000, "7d": 120_000 };
/** Rows a side's panel lists before it points to the request log for the rest. */
const PANEL_ROWS = 12;

const pct = (part: number, whole: number) => {
  const v = whole ? (part / whole) * 100 : 0;
  return `${v.toFixed(v < 1 ? 2 : 1)}%`;
};

function agoShort(ms: number, now: number) {
  const s = Math.max(0, (now - ms) / 1000);
  if (s < 3600) return `${Math.max(1, Math.round(s / 60))}m`;
  if (s < 86_400) return `${Math.round(s / 3600)}h`;
  return `${Math.round(s / 86_400)}d`;
}

/** One repeated failure: what it was and on which model, its share of the side, then who and why. */
function GroupRow({ g, top, range, now }: { g: UsageFailureGroup; top: number; range: FailureRange; now: number }) {
  const hint = isErrorClass(g.error_class) ? ERROR_CLASSES[g.error_class].hint : undefined;
  const reason = g.reason.trim() || `HTTP ${g.status_code}`;
  const who = hasKey(g) ? g.key_name || `${g.key_id.slice(0, 8)}…` : g.tenant_name || "No key";
  return (
    <li className="grid grid-cols-[minmax(0,1fr)_3.75rem_2.75rem] items-center gap-3 border-t border-border px-[18px] py-[9px] text-[13px]">
      <span className="flex min-w-0 flex-col gap-1.5">
        <span className="flex min-w-0 items-baseline gap-2">
          <span className="shrink-0 font-medium" title={hint}>{classLabel(g.error_class) || "Not classified"}</span>
          <span className="truncate font-mono text-xs text-muted-foreground" title={g.model}>{g.model || "—"}</span>
        </span>
        <Meter value={g.requests} max={top} strong={g.platform} className="h-[3px]" />
        <span className="truncate text-xs text-muted-foreground" title={`${who}${g.tenant_name && hasKey(g) ? ` (${g.tenant_name})` : ""}: ${reason}`}>
          <span className="font-mono">{g.status_code}</span> · {who} · {reason}
        </span>
      </span>
      <Link href={groupLogsHref(g, range)} title="See these requests" className="text-right font-mono text-xs tabular-nums underline-offset-2 hover:underline">{compact(g.requests)}</Link>
      <span className="text-right font-mono text-xs text-muted-foreground" title={`First ${seenAgo(g.first_ms, now)}, last ${seenAgo(g.last_ms, now)}`}>{agoShort(g.last_ms, now)}</span>
    </li>
  );
}

/** One side's repeated failures, largest first, like the Overview's rankings. */
function SidePanel({ title, subtitle, href, groups, range, now, empty, loading }: {
  title: string; subtitle: string; href: string; groups: UsageFailureGroup[]; range: FailureRange; now: number; empty: string; loading: boolean;
}) {
  const top = groups[0]?.requests ?? 0;
  const rest = groups.slice(PANEL_ROWS);
  return (
    <Panel title={title} subtitle={subtitle} action={<PanelLink href={href}>Requests</PanelLink>}>
      <div className="mt-3 pb-2">
        {groups.length === 0 ? (
          <p className="border-t border-border px-[18px] py-6 text-center text-sm text-muted-foreground">
            {loading ? <span className="skeleton mx-auto block h-4 w-40 rounded" /> : empty}
          </p>
        ) : (
          <>
            <div className="grid grid-cols-[minmax(0,1fr)_3.75rem_2.75rem] gap-3 border-t border-border px-[18px] py-2 text-[11.5px] text-muted-foreground">
              <span>Failure</span><span className="text-right">Count</span><span className="text-right" title="Last seen">Last</span>
            </div>
            <ul>
              {groups.slice(0, PANEL_ROWS).map((g) => (
                <GroupRow key={`${g.error_class}:${g.key_id}:${g.model}:${g.status_code}:${g.reason}:${g.first_ms}`} g={g} top={top} range={range} now={now} />
              ))}
            </ul>
            {rest.length > 0 && (
              <Link href={href} className="block border-t border-border px-[18px] py-[9px] text-[13px] text-muted-foreground hover:text-foreground">
                {rest.length} more kind{rest.length === 1 ? "" : "s"} of failure · {compact(rest.reduce((n, g) => n + g.requests, 0))} requests
              </Link>
            )}
          </>
        )}
      </div>
    </Panel>
  );
}

/**
 * Which requests failed and whose side each failure was on: the platform's
 * (it could not serve a valid request) or the caller's (the request could
 * not be served as sent). The same failure repeated shows as one row.
 */
export function FailuresView({ initialRange, initial }: { initialRange: FailureRange; initial: UsageFailureGroups | null }) {
  const [range, setRange] = useState<FailureRange>(initialRange);
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
  const bucketMs = Math.round(rangeOf(range).ms / TREND_POINTS);
  const histogram = useQuery({
    queryKey: ["usage-failures-trend", range],
    queryFn: async () => {
      const since = Date.now() - rangeOf(range).ms;
      return { since, data: await getJson<UsageLogHistogram>(`/api/live/usage/logs/histogram?since_ms=${since}&bucket_ms=${bucketMs}`) };
    },
    placeholderData: keepPreviousData,
    refetchInterval: POLL_MS[range],
  });
  const data = query.data;
  const callers = data ? Math.max(0, data.failures - data.platform_failures) : 0;
  const words = rangeOf(range).words;
  const trend = (pick: (b: UsageLogHistogram["buckets"][number]) => number) =>
    histogram.data ? trendOf(histogram.data.data.buckets, histogram.data.since, histogram.data.data.bucket_ms, pick) : [];

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
          <button type="button" onClick={() => { void query.refetch(); void histogram.refetch(); }} aria-label="Refresh" title="Refresh" className="flex h-[34px] w-[34px] items-center justify-center rounded-lg border border-border text-muted-foreground hover:bg-accent hover:text-foreground">
            <RefreshCw className={cn("h-3.5 w-3.5", (query.isFetching || histogram.isFetching) && "animate-spin")} />
          </button>
        </div>
      </div>

      {data && (
        <div className={cn("grid gap-3 sm:grid-cols-3", query.isPlaceholderData && "opacity-60")}>
          <Kpi
            label={`Platform failures · ${range}`}
            value={pct(data.platform_failures, data.requests)}
            detail={`${data.platform_failures.toLocaleString()} of ${data.requests.toLocaleString()} requests`}
            trend={trend((b) => b.platform_errors ?? 0)}
            href={logsHref({ fault: "platform", window: range })}
            emphasis={data.platform_failures > 0}
          />
          <Kpi
            label={`Caller failures · ${range}`}
            value={pct(callers, data.requests)}
            detail={`${callers.toLocaleString()} of ${data.requests.toLocaleString()} requests`}
            trend={trend((b) => b.errors - (b.platform_errors ?? 0))}
            href={logsHref({ fault: "client", window: range })}
          />
          <Kpi
            label={`Requests · ${range}`}
            value={compact(data.requests)}
            detail={`${data.failures.toLocaleString()} failed`}
            trend={trend((b) => b.requests)}
            href={logsHref({ window: range })}
          />
        </div>
      )}

      <div className={cn("grid gap-4 lg:grid-cols-2", query.isPlaceholderData && "opacity-60")}>
        <SidePanel
          title="On the platform's side"
          subtitle="The gateway or a model server could not serve a valid request"
          href={logsHref({ fault: "platform", window: range })}
          groups={data ? groupsFor(data.groups, "platform") : []}
          range={range}
          now={now}
          empty={data ? `Nothing failed on the platform's side in ${words}.` : "The failures could not be read."}
          loading={query.isLoading}
        />
        <SidePanel
          title="From callers"
          subtitle="Refused as sent, hung up, retired models, or over a limit"
          href={logsHref({ fault: "client", window: range })}
          groups={data ? groupsFor(data.groups, "client") : []}
          range={range}
          now={now}
          empty={data ? `No caller's request failed in ${words}.` : "The failures could not be read."}
          loading={query.isLoading}
        />
      </div>
    </div>
  );
}
