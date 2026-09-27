"use client";

import { memo, useMemo, useState } from "react";
import { Area, AreaChart, Bar, BarChart, CartesianGrid, ResponsiveContainer, Tooltip, XAxis, YAxis } from "recharts";
import { axisTick, chartGrid, compactAxis, timeCursor } from "@/components/chart-tooltip";
import { GROUP_TONES, MAX_CHART_ROWS, OTHER_TONE, thinHistory, type GroupHistoryPoint, type GroupKey } from "@/lib/fairshare-model";
import { Panel, Segmented } from "@/components/overview/ui";

type Window = "15m" | "1h";
const WINDOW_MS: Record<Window, number> = { "15m": 900_000, "1h": 3_600_000 };

interface Series { key: string; name: string; tone: string }

function HistoryTooltip({ active, payload, series }: { active?: boolean; payload?: { payload: GroupHistoryPoint }[]; series: Series[] }) {
  const row = payload?.[0]?.payload;
  if (!active || !row) return null;
  const total = series.reduce((n, s) => n + Number(row[s.key] ?? 0), 0);
  return (
    <div className="min-w-44 rounded-lg border border-border bg-[hsl(240_5%_9%)] px-3 py-2 text-xs shadow-xl">
      <p className="mb-1.5 font-mono text-muted-foreground">{row.time}</p>
      {[...series].reverse().map((s) => (
        <div key={s.key} className="flex items-center gap-2"><span className="h-2 w-2 rounded-sm" style={{ background: s.tone }} /><span className="text-muted-foreground">{s.name}</span><span className="ml-auto pl-6 font-mono tabular-nums">{Number(row[s.key] ?? 0).toLocaleString()}</span></div>
      ))}
      <div className="mt-1 flex justify-between gap-6 border-t border-border pt-1"><span>Running</span><span className="font-mono tabular-nums">{total.toLocaleString()}</span></div>
      <div className="flex justify-between gap-6 text-muted-foreground"><span>Waiting</span><span className="font-mono tabular-nums">{Number(row.queued).toLocaleString()}</span></div>
    </div>
  );
}

/**
 * Requests holding a slot, stacked by group, with waiting requests as their
 * own chart underneath: two measures, two axes' worth of meaning, so two
 * charts sharing time and hover rather than one chart with a second axis.
 */
export const RunningHistory = memo(function RunningHistory({ history, groups, oldestTs, retentionMs, scopeLabel }: {
  history: GroupHistoryPoint[];
  groups: GroupKey[];
  oldestTs: number | null;
  retentionMs: number | null;
  scopeLabel: string;
}) {
  const [window, setWindow] = useState<Window>("1h");
  const { rows, series, peak } = useMemo(() => {
    const newest = history.at(-1)?.ts ?? 0;
    const inWindow = history.filter((r) => r.ts >= newest - WINDOW_MS[window]);
    // The heaviest groups keep their own tone; the rest fold into "Other".
    const volume = new Map(groups.map((g) => [g.key, inWindow.reduce((n, r) => n + Number(r[g.key] ?? 0), 0)]));
    const ranked = [...groups].sort((a, b) => (volume.get(b.key) ?? 0) - (volume.get(a.key) ?? 0));
    const kept = ranked.slice(0, GROUP_TONES.length);
    const folded = ranked.slice(GROUP_TONES.length);
    const series: Series[] = kept.map((g, i) => ({ key: g.key, name: g.name, tone: GROUP_TONES[i] }));
    let rows = inWindow;
    if (folded.length) {
      series.push({ key: "group:__other", name: `Other (${folded.length})`, tone: OTHER_TONE });
      rows = inWindow.map((r) => ({ ...r, "group:__other": folded.reduce((n, g) => n + Number(r[g.key] ?? 0), 0) }));
    }
    const peak = rows.reduce<GroupHistoryPoint | null>((best, r) => (r.queued > 0 && (!best || r.queued > best.queued) ? r : best), null);
    return { rows: thinHistory(rows, MAX_CHART_ROWS), series, peak };
  }, [history, groups, window]);

  const disabled = retentionMs === 0;
  const subtitle = disabled
    ? "History disabled (OBLETH_FAIRSHARE_HISTORY_SECS=0)"
    : oldestTs
      ? `Requests holding a slot, stacked by group · ${scopeLabel} · History since ${new Date(oldestTs).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })}`
      : `Requests holding a slot, stacked by group · ${scopeLabel}`;

  return (
    <Panel
      label="Running history"
      title={`Running over the last ${window === "1h" ? "hour" : "15 minutes"}`}
      subtitle={subtitle}
      action={
        <div className="flex flex-wrap items-center gap-3">
          {series.length > 0 && (
            <span className="flex flex-wrap gap-3 text-xs text-muted-foreground">
              {series.map((s) => <span key={s.key} className="inline-flex items-center gap-1.5"><span className="h-2.5 w-2.5 rounded-sm" style={{ background: s.tone }} />{s.name}</span>)}
            </span>
          )}
          <Segmented label="History window" value={window} onChange={setWindow} options={[{ value: "15m", label: "15m" }, { value: "1h", label: "1h" }]} />
        </div>
      }
    >
      <div className="flex flex-col gap-1 px-[18px] pb-4 pt-3">
        {rows.length === 0 ? (
          <div className="flex h-[200px] items-center justify-center rounded-lg border border-dashed border-border text-sm text-muted-foreground">
            {disabled ? "History is turned off on the gateway." : "No samples yet"}
          </div>
        ) : (
          <>
            <div className="h-[160px]">
              <ResponsiveContainer width="100%" height="100%">
                <AreaChart data={rows} syncId="fairshare-history" margin={{ top: 6, right: 4, left: 0, bottom: 0 }}>
                  <CartesianGrid {...chartGrid} vertical={false} />
                  <XAxis dataKey="time" hide />
                  <YAxis tick={axisTick} axisLine={false} tickLine={false} width={44} allowDecimals={false} tickFormatter={compactAxis} />
                  <Tooltip cursor={timeCursor} content={<HistoryTooltip series={series} />} />
                  {series.map((s) => (
                    <Area key={s.key} type="monotone" dataKey={s.key} name={s.name} stackId="running" stroke="hsl(240 5% 7%)" strokeWidth={2} fill={s.tone} fillOpacity={1} isAnimationActive={false} />
                  ))}
                </AreaChart>
              </ResponsiveContainer>
            </div>
            <div className="flex items-center justify-between pl-11 pt-1 text-xs">
              <span className="text-secondary-foreground">Waiting</span>
              <span className="text-muted-foreground">{peak ? <>Peak <span className="text-foreground">{peak.queued.toLocaleString()}</span> at {peak.time}</> : "Nothing waited in this window"}</span>
            </div>
            <div className="h-[60px]">
              <ResponsiveContainer width="100%" height="100%">
                <BarChart data={rows} syncId="fairshare-history" margin={{ top: 2, right: 4, left: 0, bottom: 0 }} barCategoryGap={1}>
                  <XAxis dataKey="time" tick={axisTick} axisLine={false} tickLine={false} minTickGap={56} height={20} />
                  <YAxis width={44} tick={false} axisLine={false} tickLine={false} />
                  <Tooltip cursor={{ fill: "hsl(240 4% 16% / 0.5)" }} content={<HistoryTooltip series={series} />} />
                  <Bar dataKey="queued" name="Waiting" fill={GROUP_TONES[0]} radius={[2, 2, 0, 0]} isAnimationActive={false} />
                </BarChart>
              </ResponsiveContainer>
            </div>
          </>
        )}
      </div>
    </Panel>
  );
});
