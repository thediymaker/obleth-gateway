"use client";

import { memo, useMemo, useState } from "react";
import { ChevronDown } from "lucide-react";
import { compact, formatMs, modelTypeLabel, RANGE_LABEL, type FleetTile, type OverviewRange } from "@/lib/overview-model";
import { cn } from "@/lib/utils";
import { HealthGlyph, Meter, Panel, PanelLink, Pill, SectionLabel, Segmented } from "./ui";

const FIRST_PAGE = 24;

function tileNote(t: FleetTile): string {
  if (t.attention === "down") {
    const h = t.healthRow;
    return [h?.consecutive_failures ? `${h.consecutive_failures} failed checks` : "Failing", h?.last_http_status ? `HTTP ${h.last_http_status}` : null].filter(Boolean).join(" · ");
  }
  if (t.attention === "full") return `${t.inFlight}/${t.cap} · ${t.queued} queued`;
  if (t.health === "maintenance") {
    const until = t.healthRow?.maintenance_until ? new Date(t.healthRow.maintenance_until).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" }) : null;
    return until ? `Maintenance · ${until}` : "Maintenance";
  }
  if (t.health === "unknown" && !t.requests) return "Awaiting first check";
  if (!t.requests) return "No traffic";
  return `${compact(t.requests)} req${t.p50TtftMs ? ` · ${formatMs(t.p50TtftMs)}` : ""}`;
}

/**
 * Every enabled model at a glance: health as a shape, load as a bar
 * (in flight ÷ pool size), and its traffic in the selected range.
 */
export const Fleet = memo(function Fleet({ fleet, range, onOpen }: { fleet: FleetTile[]; range: OverviewRange; onOpen: (name: string) => void }) {
  const [filter, setFilter] = useState("all");
  const [expanded, setExpanded] = useState(false);
  const types = useMemo(() => {
    const counts = new Map<string, number>();
    for (const t of fleet) counts.set(modelTypeLabel(t.type), (counts.get(modelTypeLabel(t.type)) ?? 0) + 1);
    return [...counts.entries()].sort((a, b) => b[1] - a[1]);
  }, [fleet]);
  const attention = fleet.filter((t) => t.attention).length;
  const shown = fleet.filter((t) => filter === "all" || (filter === "attention" ? !!t.attention : modelTypeLabel(t.type) === filter));
  const visible = expanded ? shown : shown.slice(0, FIRST_PAGE);

  return (
    <Panel
      title="Model fleet"
      subtitle={`Every enabled model: health, load now, and traffic over the ${RANGE_LABEL[range].toLowerCase()}`}
      action={
        <Segmented
          label="Model type"
          value={filter}
          onChange={(v) => { setFilter(v); setExpanded(false); }}
          options={[
            { value: "all", label: `All ${fleet.length}` },
            ...(attention ? [{ value: "attention", label: `Needs attention ${attention}` }] : []),
            ...(types.length > 1 ? types.map(([label, n]) => ({ value: label, label: `${label} ${n}` })) : []),
          ]}
        />
      }
    >
      {fleet.length === 0 ? (
        <p className="m-[18px] rounded-lg border border-dashed border-border p-6 text-center text-sm text-muted-foreground">No enabled models yet.</p>
      ) : (
        <ul className="grid grid-cols-2 gap-2 px-[18px] pb-1 pt-3.5 sm:grid-cols-3 md:grid-cols-4 xl:grid-cols-6 2xl:grid-cols-8">
          {visible.map((t) => {
            const down = t.attention === "down";
            return (
              <li key={t.id} className="min-w-0">
                <button
                  type="button"
                  onClick={() => onOpen(t.name)}
                  aria-label={`${t.name}: ${tileNote(t)}. Open details`}
                  className={cn(
                    "flex w-full min-w-0 flex-col gap-[7px] rounded-[9px] border px-[11px] py-2.5 text-left transition-colors focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring",
                    down ? "border-foreground bg-foreground text-background" : t.attention === "full" ? "border-muted-foreground bg-background hover:bg-accent/40" : "border-border bg-background hover:border-muted-foreground/60 hover:bg-accent/40",
                  )}
                >
                  <span className="flex min-w-0 items-center gap-1.5">
                    <span className={cn("min-w-0 flex-1 truncate text-[12.5px]", down && "font-semibold")} title={t.name}>{t.name}</span>
                    {down ? <span className="text-[10.5px] font-bold tracking-wide">DOWN</span> : t.attention === "full" ? <span className="text-[10.5px] font-bold tracking-wide">FULL</span> : <HealthGlyph state={t.health} />}
                  </span>
                  {down ? <span className="block h-1 rounded-full bg-background/40" aria-hidden="true" /> : <Meter value={t.inFlight} max={t.cap} className="h-1" strong={t.attention === "full"} />}
                  <span className={cn("truncate font-mono text-[11px]", down ? "text-background/70" : "text-muted-foreground")}>{tileNote(t)}</span>
                </button>
              </li>
            );
          })}
        </ul>
      )}
      <div className="flex flex-wrap items-center justify-between gap-3 px-[18px] pb-4 pt-2.5">
        {shown.length > FIRST_PAGE ? (
          <button type="button" onClick={() => setExpanded(!expanded)} className="inline-flex items-center gap-1.5 text-[12.5px] text-secondary-foreground hover:text-foreground">
            {expanded ? "Show fewer" : `Show ${shown.length - FIRST_PAGE} more`}<ChevronDown className={cn("h-3.5 w-3.5 transition-transform", expanded && "rotate-180")} aria-hidden="true" />
          </button>
        ) : <span />}
        <span className="flex flex-wrap items-center gap-3 text-[11.5px] text-muted-foreground">
          <span className="inline-flex items-center gap-1.5"><HealthGlyph state="healthy" />Healthy</span>
          <span className="inline-flex items-center gap-1.5"><HealthGlyph state="unknown" />No data</span>
          <span className="inline-flex items-center gap-1.5"><HealthGlyph state="maintenance" />Maintenance</span>
          <span className="inline-flex items-center gap-1.5"><HealthGlyph state="unhealthy" />Failing</span>
          <span>· bar = in flight ÷ pool size</span>
        </span>
      </div>
    </Panel>
  );
});

export interface LiveCapacity {
  inFlight: number;
  slots: number;
  queued: number;
  replicas: number;
  shared: boolean;
  activeTenants: number;
  belowShare: number;
}

export function RightNow({ capacity, fleet, onOpen }: { capacity: LiveCapacity; fleet: FleetTile[]; onOpen: (name: string) => void }) {
  const busiest = fleet.filter((t) => t.inFlight > 0 && t.cap > 0).sort((a, b) => b.inFlight / b.cap - a.inFlight / a.cap || b.inFlight - a.inFlight).slice(0, 6);
  const pct = capacity.slots > 0 ? Math.round((capacity.inFlight / capacity.slots) * 100) : 0;
  return (
    <Panel
      title="Right now"
      subtitle={capacity.replicas > 1 ? `${capacity.shared ? "Cluster-wide across" : "This gateway, one of"} ${capacity.replicas} gateways` : "This gateway"}
      action={<Pill><HealthGlyph state="healthy" className="h-1.5 w-1.5" />Live</Pill>}
    >
      <div className="flex flex-col gap-2.5 px-[18px] pt-4">
        <div className="flex items-baseline gap-2.5">
          <span className="text-[40px] font-semibold leading-none tabular-nums">{capacity.inFlight.toLocaleString()}</span>
          <span className="text-[13px] text-muted-foreground">in flight of {capacity.slots.toLocaleString()} slots</span>
          <span className="ml-auto font-mono text-[13px]">{pct}%</span>
        </div>
        <Meter value={capacity.inFlight} max={capacity.slots} className="h-2" />
        <div className="grid grid-cols-3 gap-2 pt-1">
          <div><div className="text-lg font-semibold tabular-nums">{capacity.queued.toLocaleString()}</div><div className="text-[11.5px] text-muted-foreground">queued</div></div>
          <div><div className="text-lg font-semibold tabular-nums">{capacity.activeTenants.toLocaleString()}</div><div className="text-[11.5px] text-muted-foreground">tenants active</div></div>
          <div><div className="text-lg font-semibold tabular-nums">{capacity.belowShare.toLocaleString()}</div><div className="text-[11.5px] text-muted-foreground">below fair share</div></div>
        </div>
      </div>
      <SectionLabel className="px-[18px] pb-2 pt-[18px]">Busiest models</SectionLabel>
      <div className="flex flex-col pb-2">
        {busiest.length === 0 && <p className="px-[18px] py-2 text-[12.5px] text-muted-foreground">Nothing in flight.</p>}
        {busiest.map((t) => (
          <button key={t.id} type="button" onClick={() => onOpen(t.name)} className="grid grid-cols-[minmax(0,9.5rem)_minmax(0,1fr)_3.5rem] items-center gap-3 px-[18px] py-[7px] text-left hover:bg-accent/40 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-inset focus-visible:ring-ring">
            <span className="truncate text-[12.5px]" title={t.name}>{t.name}</span>
            <Meter value={t.inFlight} max={t.cap} strong={t.attention === "full"} />
            <span className="text-right font-mono text-xs tabular-nums">{t.inFlight}/{t.cap}</span>
          </button>
        ))}
      </div>
      <div className="mt-auto flex items-center justify-between border-t border-border px-[18px] pb-4 pt-3">
        <PanelLink href="/fairshare">Open Fairshare</PanelLink>
        <span className="text-xs text-muted-foreground">{capacity.shared ? "Shared slots" : capacity.replicas > 1 ? "Split per gateway" : "Single gateway"}</span>
      </div>
    </Panel>
  );
}
