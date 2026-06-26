"use client";

import {
  Activity,
  AlertTriangle,
  Gauge,
  KeyRound,
} from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { CodeBlock, CopyButton } from "@/components/portal/copy-button";
import type { KeyUsageSummary, UsageAgg, UsageLogEntry } from "@/lib/obleth";
import { cn, formatCurrency, formatNumber } from "@/lib/utils";

export function PortalUsage({
  usage,
  keyUsage,
  recent,
  gatewayBase,
}: {
  usage: UsageAgg[];
  keyUsage: KeyUsageSummary[];
  recent: UsageLogEntry[];
  gatewayBase: string;
}) {
  const total = usage.reduce(
    (acc, row) => ({
      requests: acc.requests + row.requests,
      input_tokens: acc.input_tokens + row.input_tokens,
      output_tokens: acc.output_tokens + row.output_tokens,
      total_tokens: acc.total_tokens + row.total_tokens,
    }),
    { requests: 0, input_tokens: 0, output_tokens: 0, total_tokens: 0 },
  );
  const recentErrors = recent.filter((row) => row.status_code >= 400).length;
  const totalCost = keyUsage.reduce((sum, row) => sum + Number(row.cost_usd ?? 0), 0);
  const activeKeys = keyUsage.filter((row) => Number(row.requests ?? 0) > 0).length;
  const headerSnippet = [
    `curl -i ${gatewayBase}/v1/chat/completions`,
    `  -H "Authorization: Bearer $OBLETH_API_KEY"`,
    `  -H "Content-Type: application/json"`,
    `  -H "X-Session-ID: notebook-demo"`,
    `  -d '{"model":"model-name","messages":[{"role":"user","content":"ping"}]}'`,
  ].join(" \\\n");

  return (
    <div className="space-y-6">
      <div>
        <h1 className="text-lg font-semibold tracking-tight">Usage</h1>
        <p className="text-sm text-muted-foreground">
          Tenant request volume, token totals, key activity, and recent gateway responses.
        </p>
      </div>

      <div className="grid grid-cols-2 gap-3 lg:grid-cols-4">
        <StatCard icon={Activity} label="Requests" value={formatNumber(total.requests)} hint="current reporting window" />
        <StatCard icon={Gauge} label="Tokens" value={formatNumber(total.total_tokens)} hint={`${formatNumber(total.input_tokens)} in / ${formatNumber(total.output_tokens)} out`} />
        <StatCard icon={KeyRound} label="Active keys" value={formatNumber(activeKeys)} hint={`${formatNumber(keyUsage.length)} with summaries`} />
        <StatCard icon={AlertTriangle} label="Recent errors" value={formatNumber(recentErrors)} hint="last 24 hours" tone={recentErrors > 0 ? "bad" : undefined} />
      </div>

      <div className="grid gap-4 xl:grid-cols-[minmax(0,0.9fr)_minmax(0,1.1fr)]">
        <Card>
          <CardHeader>
            <CardTitle>Request identifiers</CardTitle>
            <CardDescription>
              Add a session header and keep the `x-obleth-request-id` response header for support.
            </CardDescription>
          </CardHeader>
          <CardContent>
            <CodeBlock label="curl with request headers" code={headerSnippet} />
          </CardContent>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle>Key activity</CardTitle>
            <CardDescription>Recent request totals grouped by API key.</CardDescription>
          </CardHeader>
          <CardContent>
            {keyUsage.length === 0 ? (
              <div className="rounded-lg border border-dashed border-border/70 px-6 py-10 text-center text-sm text-muted-foreground">
                No key activity in the current window.
              </div>
            ) : (
              <div className="space-y-2">
                {keyUsage.slice(0, 8).map((row) => (
                  <KeyUsageRow key={row.key_id} row={row} />
                ))}
              </div>
            )}
          </CardContent>
        </Card>
      </div>

      <Card>
        <CardHeader>
          <CardTitle>Recent requests</CardTitle>
          <CardDescription>
            {formatNumber(recent.length)} newest request{recent.length === 1 ? "" : "s"} from the last 24 hours.
          </CardDescription>
        </CardHeader>
        <CardContent className="p-0">
          {recent.length === 0 ? (
            <div className="px-6 py-12 text-center text-sm text-muted-foreground">
              No recent requests found.
            </div>
          ) : (
            <>
              <div className="hidden overflow-x-auto md:block">
                <table className="w-full min-w-[820px] text-sm">
                  <thead>
                    <tr className="border-y border-border bg-card/40 text-left text-xs text-muted-foreground">
                      <th className="px-4 py-2 font-medium">Time</th>
                      <th className="px-3 py-2 font-medium">Status</th>
                      <th className="px-3 py-2 font-medium">Model</th>
                      <th className="px-3 py-2 font-medium">Key</th>
                      <th className="px-3 py-2 text-right font-medium">Tokens</th>
                      <th className="px-3 py-2 text-right font-medium">Duration</th>
                      <th className="px-4 py-2 font-medium">Request ID</th>
                    </tr>
                  </thead>
                  <tbody>
                    {recent.map((row) => (
                      <RecentRow key={`${row.request_id}-${row.ts_ms}`} row={row} />
                    ))}
                  </tbody>
                </table>
              </div>

              <div className="divide-y divide-border/60 md:hidden">
                {recent.map((row) => (
                  <RecentCard key={`${row.request_id}-${row.ts_ms}`} row={row} />
                ))}
              </div>
            </>
          )}
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>Window total</CardTitle>
          <CardDescription>Total tenant usage returned by the gateway summary endpoint.</CardDescription>
        </CardHeader>
        <CardContent>
          <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-5">
            <MiniMetric label="Requests" value={formatNumber(total.requests)} />
            <MiniMetric label="Input tokens" value={formatNumber(total.input_tokens)} />
            <MiniMetric label="Output tokens" value={formatNumber(total.output_tokens)} />
            <MiniMetric label="Total tokens" value={formatNumber(total.total_tokens)} />
            <MiniMetric label="Estimated cost" value={formatCurrency(totalCost)} />
          </div>
        </CardContent>
      </Card>
    </div>
  );
}

function RecentRow({ row }: { row: UsageLogEntry }) {
  const ok = row.status_code >= 200 && row.status_code < 400;
  return (
    <tr className="border-b border-border/60 last:border-0">
      <td className="px-4 py-2 text-xs text-muted-foreground">
        <TimeStamp ms={row.ts_ms} />
      </td>
      <td className="px-3 py-2">
        <StatusBadge ok={ok} statusCode={row.status_code} />
      </td>
      <td className="px-3 py-2">
        <span className="block max-w-[16rem] truncate font-mono text-xs">{row.model}</span>
      </td>
      <td className="px-3 py-2">
        <span className="block max-w-[12rem] truncate text-xs">{row.key_name || row.key_prefix || "unknown"}</span>
      </td>
      <td className="px-3 py-2 text-right tabular-nums text-muted-foreground">{formatNumber(row.total_tokens)}</td>
      <td className="px-3 py-2 text-right tabular-nums text-muted-foreground">{formatSeconds(row.total_ms)}</td>
      <td className="px-4 py-2">
        <div className="flex items-center gap-2">
          <span className="font-mono text-xs text-muted-foreground">{row.request_id.slice(0, 8)}</span>
          <CopyButton value={row.request_id} label="Copy request ID" size="icon" variant="ghost" />
        </div>
      </td>
    </tr>
  );
}

function RecentCard({ row }: { row: UsageLogEntry }) {
  const ok = row.status_code >= 200 && row.status_code < 400;
  return (
    <div className="px-4 py-4">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <div className="flex flex-wrap items-center gap-2">
            <StatusBadge ok={ok} statusCode={row.status_code} />
            <Badge className="capitalize">{row.request_type || "other"}</Badge>
          </div>
          <p className="mt-2 truncate font-mono text-xs" title={row.model}>{row.model}</p>
        </div>
        <TimeStamp ms={row.ts_ms} align="right" />
      </div>
      <div className="mt-3 grid grid-cols-2 gap-2 text-xs">
        <MiniMetric label="Tokens" value={formatNumber(row.total_tokens)} />
        <MiniMetric label="Duration" value={formatSeconds(row.total_ms)} />
        <MiniMetric label="Cost" value={formatCurrency(row.cost_usd)} />
        <MiniMetric label="Key" value={row.key_name || row.key_prefix || "unknown"} />
      </div>
      <div className="mt-3 flex items-center gap-2">
        <span className="min-w-0 truncate font-mono text-xs text-muted-foreground">{row.request_id}</span>
        <CopyButton value={row.request_id} label="Copy request ID" size="icon" variant="ghost" />
      </div>
    </div>
  );
}

function KeyUsageRow({ row }: { row: KeyUsageSummary }) {
  return (
    <div className="grid gap-3 rounded-lg border border-border/70 bg-background/35 px-3 py-3 sm:grid-cols-[minmax(0,1fr)_auto] sm:items-center">
      <div className="min-w-0">
        <div className="flex min-w-0 items-center gap-2">
          <p className="truncate font-medium">{row.last_model || "No recent model"}</p>
          {row.last_status_code > 0 && (
            <Badge className={row.last_status_code >= 400 ? "border-destructive/40 text-destructive" : "border-emerald-500/40 text-emerald-500"}>
              HTTP {row.last_status_code}
            </Badge>
          )}
        </div>
        <p className="mt-0.5 truncate font-mono text-[11px] text-muted-foreground">
          {row.key_id.slice(0, 8)} / {formatLastUsed(row.last_used_ms)}
        </p>
      </div>
      <div className="grid grid-cols-3 gap-2 text-xs">
        <MiniMetric label="Requests" value={formatNumber(row.requests)} />
        <MiniMetric label="Tokens" value={formatNumber(row.total_tokens)} />
        <MiniMetric label="Cost" value={formatCurrency(row.cost_usd)} />
      </div>
    </div>
  );
}

function StatCard({
  icon: Icon,
  label,
  value,
  hint,
  tone,
}: {
  icon: typeof Activity;
  label: string;
  value: string;
  hint: string;
  tone?: "bad";
}) {
  return (
    <div className="rounded-lg border border-border bg-card p-4">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <p className="text-xs text-muted-foreground">{label}</p>
          <p className="mt-1 text-2xl font-semibold tabular-nums">{value}</p>
          <p className={cn("mt-0.5 text-[11px]", tone === "bad" ? "text-destructive" : "text-muted-foreground")}>
            {hint}
          </p>
        </div>
        <span className="flex h-8 w-8 shrink-0 items-center justify-center rounded-md border border-border/70 bg-background/40 text-muted-foreground">
          <Icon className="h-4 w-4" aria-hidden />
        </span>
      </div>
    </div>
  );
}

function MiniMetric({ label, value }: { label: string; value: string }) {
  return (
    <div className="min-w-0 rounded-md border border-border/70 bg-background/35 px-2.5 py-2">
      <p className="text-[10px] uppercase tracking-wider text-muted-foreground">{label}</p>
      <p className="mt-0.5 truncate text-xs font-medium tabular-nums">{value}</p>
    </div>
  );
}

function StatusBadge({ ok, statusCode }: { ok: boolean; statusCode: number }) {
  return (
    <span
      className={cn(
        "inline-flex items-center rounded-sm px-1.5 py-0.5 text-[11px] font-medium",
        ok
          ? "bg-[hsl(158_45%_45%/0.16)] text-[hsl(158_55%_60%)]"
          : "bg-[hsl(350_65%_55%/0.16)] text-[hsl(350_70%_66%)]",
      )}
      title={`HTTP ${statusCode}`}
    >
      {ok ? "Success" : `Error ${statusCode}`}
    </span>
  );
}

function TimeStamp({ ms, align = "left" }: { ms: number; align?: "left" | "right" }) {
  const date = new Date(ms);
  return (
    <span className={cn("flex flex-col text-xs leading-tight", align === "right" ? "items-end text-right" : "items-start")}>
      <span>{date.toLocaleDateString([], { month: "2-digit", day: "2-digit", year: "2-digit" })}</span>
      <span>{date.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" })}</span>
    </span>
  );
}

function formatSeconds(ms: number): string {
  if (!ms || ms <= 0) return "--";
  return `${(ms / 1000).toFixed(2)}s`;
}

function formatLastUsed(ms?: number): string {
  if (!ms || ms <= 0) return "Never";
  const diff = Date.now() - ms;
  if (diff < 0) return "Just now";
  const min = Math.floor(diff / 60_000);
  if (min < 1) return "Just now";
  if (min < 60) return `${min}m ago`;
  const hours = Math.floor(min / 60);
  if (hours < 24) return `${hours}h ago`;
  const days = Math.floor(hours / 24);
  if (days < 7) return `${days}d ago`;
  return new Date(ms).toLocaleDateString([], { month: "short", day: "numeric", year: "numeric" });
}
