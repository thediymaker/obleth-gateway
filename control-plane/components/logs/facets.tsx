"use client";

import { useState } from "react";
import { HTTP_REASONS, type LogFilters } from "@/lib/logs-model";
import type { UsageLogFacet, UsageLogFacets } from "@/lib/obleth";
import { compact } from "@/lib/overview-model";
import { cn } from "@/lib/utils";

const SHOWN = 5;

function List({
  title,
  facets,
  label,
  onPick,
  active,
  mono,
  failuresOnly,
}: {
  title: string;
  facets: UsageLogFacet[];
  label: (f: UsageLogFacet) => string;
  onPick: (f: UsageLogFacet) => void;
  active: (f: UsageLogFacet) => boolean;
  mono?: boolean;
  failuresOnly: boolean;
}) {
  const [all, setAll] = useState(false);
  const max = Math.max(1, ...facets.map((f) => f.requests));
  const shown = all ? facets : facets.slice(0, SHOWN);
  return (
    <div className="min-w-0">
      <p className="pb-1.5 text-[11px] font-semibold uppercase tracking-[0.07em] text-muted-foreground">{title}</p>
      {facets.length === 0 && <p className="text-xs text-muted-foreground">—</p>}
      {shown.map((f) => {
        const on = active(f);
        const failShare = f.requests ? f.errors / f.requests : 0;
        return (
          <button
            key={f.value}
            type="button"
            aria-pressed={on}
            onClick={() => onPick(f)}
            title={failuresOnly ? `${f.requests.toLocaleString()} failed` : `${f.requests.toLocaleString()} requests, ${f.errors.toLocaleString()} failed`}
            className={cn("grid w-full grid-cols-[minmax(0,1fr)_auto] items-center gap-x-2 rounded-md px-1.5 py-1 text-left text-[12.5px] hover:bg-muted/40", on && "bg-secondary")}
          >
            <span className={cn("truncate", mono && "font-mono text-[12px]")}>{label(f)}</span>
            <span className="font-mono text-[11.5px] tabular-nums text-secondary-foreground">
              {compact(f.requests)}
              {!failuresOnly && f.errors > 0 && <span className="text-muted-foreground"> · {failShare >= 0.1 ? `${Math.round(failShare * 100)}%` : `${(failShare * 100).toFixed(1)}%`} failed</span>}
            </span>
            <span className="col-span-2 mt-0.5 flex h-1 overflow-hidden rounded-full bg-muted/50">
              <span className="bg-foreground" style={{ width: `${(f.errors / max) * 100}%` }} />
              <span className="bg-muted-foreground/50" style={{ width: `${((f.requests - f.errors) / max) * 100}%` }} />
            </span>
          </button>
        );
      })}
      {facets.length > SHOWN && (
        <button type="button" onClick={() => setAll((v) => !v)} className="px-1.5 pt-1 text-[11.5px] text-muted-foreground hover:text-foreground">
          {all ? "Fewer" : `${facets.length - SHOWN} more`}
        </button>
      )}
    </div>
  );
}

/**
 * What the requests in view are made of: the busiest status codes, models,
 * teams, keys and types, each clickable to narrow the list to it. Looking at
 * failures, it leads with the status code and model pairs that fail most.
 */
export function Facets({ facets, filters, onPatch, loading }: { facets: UsageLogFacets | undefined; filters: LogFilters; onPatch: (p: Partial<LogFilters>) => void; loading: boolean }) {
  const failuresOnly = filters.status === "error" || Number(filters.statusCode) >= 400;
  if (!facets) {
    return <div className={cn("rounded-xl border border-border bg-card", loading && "skeleton h-[168px]")} />;
  }
  const empty = facets.models.length === 0;
  const toggle = (key: keyof LogFilters, value: string) => onPatch({ [key]: filters[key] === value ? "" : value } as Partial<LogFilters>);
  return (
    <section aria-label={failuresOnly ? "Failures by" : "What's in this window"} className="rounded-xl border border-border bg-card px-[18px] pb-3 pt-3.5">
      <div className="flex items-baseline justify-between pb-2.5">
        <h2 className="text-sm font-semibold">{failuresOnly ? "Failures by" : "What's in this window"}</h2>
        <span className="text-[11.5px] text-muted-foreground">Click any line to see only those requests</span>
      </div>
      {empty ? (
        <p className="py-4 text-center text-xs text-muted-foreground">No requests to break down.</p>
      ) : (
        <div className="flex flex-col gap-4">
          {facets.failures.length > 0 && (
            <div>
              <p className="pb-1.5 text-[11px] font-semibold uppercase tracking-[0.07em] text-muted-foreground">Most common failures</p>
              <div className="grid gap-1 sm:grid-cols-2 xl:grid-cols-4">
                {facets.failures.slice(0, 8).map((f) => {
                  const on = filters.statusCode === String(f.status_code) && filters.model === f.model;
                  return (
                    <button
                      key={`${f.status_code}:${f.model}`}
                      type="button"
                      aria-pressed={on}
                      onClick={() => onPatch(on ? { statusCode: "", model: "" } : { statusCode: String(f.status_code), model: f.model, status: "" })}
                      className={cn("flex min-w-0 items-center gap-2 rounded-lg border border-border px-2.5 py-1.5 text-left text-[12.5px] hover:border-muted-foreground/60", on && "border-foreground bg-secondary")}
                    >
                      <span className="rounded-full bg-foreground px-1.5 font-mono text-[11px] font-semibold text-background">{f.status_code}</span>
                      <span className="min-w-0 flex-1 truncate font-mono text-[12px]">{f.model}</span>
                      <span className="font-mono text-[11.5px] text-secondary-foreground">{compact(f.requests)}</span>
                    </button>
                  );
                })}
              </div>
            </div>
          )}
          <div className="grid gap-x-6 gap-y-4 sm:grid-cols-2 lg:grid-cols-5">
            <List
              title="Status"
              facets={facets.status_codes}
              label={(f) => `${f.value}${HTTP_REASONS[Number(f.value)] ? ` ${HTTP_REASONS[Number(f.value)]}` : Number(f.value) < 400 ? " OK" : ""}`}
              active={(f) => filters.statusCode === f.value}
              onPick={(f) => toggle("statusCode", f.value)}
              failuresOnly={failuresOnly}
            />
            <List title="Model" facets={facets.models} label={(f) => f.value || "(none)"} active={(f) => filters.model === f.value} onPick={(f) => toggle("model", f.value)} mono failuresOnly={failuresOnly} />
            <List title="Team" facets={facets.tenants} label={(f) => f.label || `${f.value.slice(0, 8)}…`} active={(f) => filters.tenantId === f.value} onPick={(f) => onPatch(filters.tenantId === f.value ? { tenantId: "", keyId: "" } : { tenantId: f.value, keyId: "" })} failuresOnly={failuresOnly} />
            <List title="Key" facets={facets.keys} label={(f) => f.label || `${f.value.slice(0, 8)}…`} active={(f) => filters.keyId === f.value} onPick={(f) => toggle("keyId", f.value)} failuresOnly={failuresOnly} />
            <List title="Type" facets={facets.request_types} label={(f) => f.value || "other"} active={(f) => filters.requestType === f.value} onPick={(f) => toggle("requestType", f.value)} failuresOnly={failuresOnly} />
          </div>
        </div>
      )}
    </section>
  );
}
