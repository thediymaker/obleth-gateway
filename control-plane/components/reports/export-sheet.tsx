"use client";

import { useEffect, useMemo, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { Download } from "lucide-react";
import { Sheet } from "@/components/models/ui";
import { Button } from "@/components/ui/button";
import { Select } from "@/components/ui/select";
import type { UsageDailyGroupBy, UsageDailyRow } from "@/lib/obleth";
import {
  COLUMN_GROUPS,
  EXPORT_GROUPS,
  EXPORT_PRESETS,
  loadExportColumns,
  rangeLabel,
  storeExportColumns,
  type DayRange,
  type ExportPresetId,
} from "@/lib/reports-model";
import { ALL_COLUMNS, COLUMN_LABELS, exportCell, type ExportColumn } from "@/lib/usage-export";
import { cn, getJson } from "@/lib/utils";

const PREVIEW_ROWS = 5;

export function ExportSheet({
  open,
  onClose,
  range,
  filters,
  filterLine,
  names,
}: {
  open: boolean;
  onClose: () => void;
  range: DayRange;
  filters: { tenantId: string; keyId: string; model: string };
  filterLine: string;
  names: { tenantNames: Map<string, string>; keyNames: Map<string, string>; keyPrefixes: Map<string, string> };
}) {
  const [preset, setPreset] = useState<ExportPresetId>("team");
  const [group, setGroup] = useState<UsageDailyGroupBy>("tenant");
  const [columns, setColumns] = useState<ExportColumn[]>(EXPORT_PRESETS[0].columns);
  const [remember, setRemember] = useState(false);

  // A remembered column set comes back as Custom.
  useEffect(() => {
    const saved = loadExportColumns();
    if (saved?.length) {
      setPreset("custom");
      setColumns(saved.filter((c) => (ALL_COLUMNS as readonly string[]).includes(c)));
      setRemember(true);
    }
  }, []);

  const params = useMemo(() => {
    const p = new URLSearchParams({ start_day: range.start, end_day: range.end, group_by: group });
    if (filters.tenantId) p.set("tenant_id", filters.tenantId);
    if (filters.keyId) p.set("key_id", filters.keyId);
    if (filters.model) p.set("model", filters.model);
    return p;
  }, [range, group, filters]);

  // The rows the file will hold, for the count and the preview.
  const rows = useQuery({
    queryKey: ["usage-daily", range.start, range.end, filters.tenantId, filters.keyId, filters.model, group],
    enabled: open,
    queryFn: () => getJson<UsageDailyRow[]>(`/api/live/usage/daily?${params}`),
    staleTime: 60_000,
  });

  const ordered = ALL_COLUMNS.filter((c) => columns.includes(c));
  const ctx = { startDay: range.start, endDay: range.end, ...names };
  const pick = (id: ExportPresetId) => {
    setPreset(id);
    const p = EXPORT_PRESETS.find((x) => x.id === id);
    if (p) {
      setGroup(p.group);
      setColumns(p.columns);
    }
  };
  const toggle = (c: ExportColumn) => {
    setPreset("custom");
    setColumns((cols) => (cols.includes(c) ? cols.filter((x) => x !== c) : [...cols, c]));
  };
  const download = () => {
    storeExportColumns(remember ? ordered : null);
    const p = new URLSearchParams(params);
    p.set("columns", ordered.join(","));
    window.location.href = `/api/live/usage/export?${p}`;
    onClose();
  };
  const count = rows.data?.length;

  return (
    <Sheet
      open={open}
      onClose={onClose}
      title="Export usage"
      description={`${rangeLabel(range)} · ${filterLine} · from the daily totals, so it matches this page`}
      width="w-[min(820px,100vw)]"
      footer={
        <div className="flex flex-wrap items-center justify-between gap-3">
          <label className="flex items-center gap-2 text-[12.5px] text-muted-foreground">
            <input type="checkbox" checked={remember} onChange={(e) => setRemember(e.target.checked)} className="h-3.5 w-3.5 accent-foreground" />
            Remember these columns
          </label>
          <div className="flex gap-2">
            <Button type="button" variant="outline" onClick={onClose}>Cancel</Button>
            <Button type="button" onClick={download} disabled={ordered.length === 0}>
              <Download className="h-3.5 w-3.5" />
              Download CSV{count !== undefined ? ` · ${count.toLocaleString()} row${count === 1 ? "" : "s"}` : ""}
            </Button>
          </div>
        </div>
      }
    >
      <section className="flex flex-col gap-2.5 px-6 pb-5">
        <p className="text-[11px] font-semibold uppercase tracking-[0.07em] text-muted-foreground">Start from</p>
        <div className="grid gap-2.5 sm:grid-cols-2 lg:grid-cols-4">
          {EXPORT_PRESETS.map((p) => (
            <button key={p.id} type="button" aria-pressed={preset === p.id} onClick={() => pick(p.id)} className={cn("flex min-h-[88px] flex-col gap-1 rounded-[10px] border px-3.5 py-3 text-left transition-colors", preset === p.id ? "border-foreground bg-secondary" : "border-border hover:border-muted-foreground/60")}>
              <span className="text-[13px] font-semibold">{p.title}</span>
              <span className="text-[11.5px] leading-snug text-muted-foreground">{p.description}</span>
            </button>
          ))}
        </div>
        <div className="flex flex-wrap items-center gap-3 pt-1 text-[12.5px] text-muted-foreground">
          <span>One row</span>
          <Select aria-label="One row" value={group} onValueChange={(v) => { setGroup(v as UsageDailyGroupBy); setPreset("custom"); }} className="h-8 w-56 text-[12.5px]" options={EXPORT_GROUPS} />
          {preset === "custom" && <span>Custom</span>}
        </div>
      </section>

      <section className="flex flex-col gap-3 border-t border-border px-6 py-5">
        <div className="flex justify-between">
          <p className="text-[11px] font-semibold uppercase tracking-[0.07em] text-muted-foreground">Columns</p>
          <p className="text-xs text-muted-foreground">{ordered.length} chosen · click to add or remove</p>
        </div>
        {COLUMN_GROUPS.map((g) => (
          <div key={g.name} className="grid items-start gap-2.5 sm:grid-cols-[110px_minmax(0,1fr)]">
            <span className="pt-1.5 text-xs text-muted-foreground">{g.name}</span>
            <div className="flex flex-wrap gap-1.5">
              {g.columns.map((c) => {
                const on = columns.includes(c);
                return (
                  <button key={c} type="button" aria-pressed={on} onClick={() => toggle(c)} className={cn("inline-flex h-7 items-center gap-1.5 rounded-full border px-2.5 text-xs transition-colors", on ? "border-muted-foreground bg-secondary text-foreground" : "border-border text-muted-foreground hover:text-foreground")}>
                    {on ? "✓" : "+"} {COLUMN_LABELS[c]}
                  </button>
                );
              })}
            </div>
          </div>
        ))}
        {columns.includes("day") && !group.startsWith("day") && <p className="text-xs text-muted-foreground">Date is blank unless each row is a day: pick a per-day row above.</p>}
      </section>

      <section className="flex flex-col gap-2.5 border-t border-border px-6 py-5">
        <div className="flex justify-between">
          <p className="text-[11px] font-semibold uppercase tracking-[0.07em] text-muted-foreground">Preview</p>
          <p className="text-xs text-muted-foreground">{count === undefined ? "Counting rows…" : `first ${Math.min(PREVIEW_ROWS, count)} of ${count.toLocaleString()} rows`}</p>
        </div>
        <div className="overflow-x-auto rounded-xl border border-border">
          <table className="w-full text-xs">
            <thead>
              <tr className="text-left text-[10.5px] font-semibold uppercase tracking-[0.07em] text-muted-foreground">
                {ordered.map((c) => <th key={c} className="whitespace-nowrap px-3 py-2 font-semibold">{COLUMN_LABELS[c]}</th>)}
              </tr>
            </thead>
            <tbody>
              {(rows.data ?? []).slice(0, PREVIEW_ROWS).map((r, i) => (
                <tr key={i} className="border-t border-border/70">
                  {ordered.map((c) => <td key={c} className="max-w-[14rem] truncate whitespace-nowrap px-3 py-1.5 font-mono">{String(exportCell(r, c, ctx))}</td>)}
                </tr>
              ))}
              {rows.isLoading && <tr><td colSpan={Math.max(ordered.length, 1)} className="px-3 py-3"><div className="skeleton h-4 rounded" /></td></tr>}
              {count === 0 && <tr><td colSpan={Math.max(ordered.length, 1)} className="px-3 py-4 text-center text-muted-foreground">No usage in these days: the file would have only its header.</td></tr>}
            </tbody>
          </table>
        </div>
      </section>
    </Sheet>
  );
}
