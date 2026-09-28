"use client";

import { Fragment, useEffect, useMemo, useState, useTransition } from "react";
import Link from "next/link";
import { usePathname, useRouter } from "next/navigation";
import { Download, Search, X } from "lucide-react";
import { Tile } from "@/components/models/ui";
import { Segmented } from "@/components/overview/ui";
import { Button } from "@/components/ui/button";
import { Select } from "@/components/ui/select";
import {
  actorLabel,
  burstSummary,
  changeLine,
  dayLabel,
  fieldChanges,
  groupBursts,
  isPerson,
  KIND_LABEL,
  kindOf,
  matchesKind,
  paramsFor,
  previousOf,
  queryFor,
  RANGE_LABEL,
  tailOf,
  thingHref,
  thingName,
  toCsv,
  verbOf,
  type AuditFilters,
  type AuditGroup,
  type AuditKind,
  type AuditRange,
} from "@/lib/audit-model";
import type { AuditEntry } from "@/lib/obleth";
import { cn } from "@/lib/utils";

const PAGE = 500;

function initials(actor: string): string {
  if (!isPerson(actor)) return actor.slice(0, 2);
  const local = actor.split("@")[0];
  const parts = local.split(/[._-]+/).filter(Boolean);
  return (parts.length > 1 ? parts[0][0] + parts[1][0] : local.slice(0, 2)).toUpperCase();
}

function Who({ actor }: { actor: string }) {
  return (
    <span aria-hidden="true" className={cn("inline-flex h-6 w-6 shrink-0 items-center justify-center rounded-full text-[9.5px] font-semibold", isPerson(actor) ? "bg-secondary text-secondary-foreground" : "border border-dashed border-muted-foreground/70 text-muted-foreground")}>
      {initials(actor)}
    </span>
  );
}

function Thing({ e, names, alive }: { e: AuditEntry; names: Record<string, string>; alive: Set<string> }) {
  const name = thingName(e, names);
  if (!name) return null;
  const href = thingHref(e, names, alive);
  const mono = !["settings", "tenant"].includes(e.entity_type);
  const cls = cn(mono && "font-mono text-[12.5px]");
  if (!href) return <span className={cn(cls, !alive.has(e.entity_id) && e.entity_type !== "settings" && "text-secondary-foreground")}>{name}{!alive.has(e.entity_id) && e.entity_type !== "settings" && e.entity_type !== "gateway" ? " (deleted)" : ""}</span>;
  return <Link href={href} className={cn(cls, "underline decoration-muted-foreground/40 underline-offset-[3px] hover:decoration-foreground")}>{name}</Link>;
}

function Detail({ e, entries }: { e: AuditEntry; entries: AuditEntry[] }) {
  const [raw, setRaw] = useState(false);
  const prev = previousOf(e, entries);
  const rows = fieldChanges(e, prev);
  return (
    <div className="mb-2 ml-[104px] mr-[18px] rounded-lg border border-border bg-background px-3.5 py-2.5">
      {raw ? (
        <pre className="max-h-80 overflow-auto whitespace-pre-wrap font-mono text-[11.5px] text-secondary-foreground">{JSON.stringify(e.detail, null, 2)}</pre>
      ) : rows.length === 0 ? (
        <p className="text-[12.5px] text-muted-foreground">Nothing else was recorded.</p>
      ) : (
        rows.slice(0, 24).map((r) => (
          <div key={r.field} className="grid grid-cols-[180px_minmax(0,1fr)] gap-3 py-0.5 text-[12.5px]">
            <span className="truncate text-muted-foreground">{r.field}</span>
            <span className="min-w-0 break-words">
              {r.changed ? <><span className="text-muted-foreground line-through">{r.before}</span> → <b className="font-medium">{r.after}</b></> : <>{r.after}{r.before !== undefined && <span className="text-muted-foreground"> (unchanged)</span>}</>}
            </span>
          </div>
        ))
      )}
      <div className="flex flex-wrap justify-between gap-2 pt-1.5 text-[11.5px] text-muted-foreground">
        <span>{prev ? `Compared with the change before it, ${new Date(prev.ts).toLocaleString([], { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" })}` : "No earlier change to compare with in view"} · event #{e.id}</span>
        <button type="button" onClick={() => setRaw((v) => !v)} className="text-secondary-foreground underline underline-offset-[3px] hover:text-foreground">{raw ? "Fields" : "Raw JSON"}</button>
      </div>
    </div>
  );
}

function GroupLine({ g, names, alive, entries, open, onToggle }: { g: AuditGroup; names: Record<string, string>; alive: Set<string>; entries: AuditEntry[]; open: boolean; onToggle: () => void }) {
  const e = g.events[0];
  const time = new Date(e.ts).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  const single = g.events.length === 1;
  const sum = burstSummary(g, names);
  const tail = single ? tailOf(e, names) : null;
  const change = single ? changeLine(e, previousOf(e, entries)) : null;
  return (
    <>
      <div className={cn("grid min-h-[44px] grid-cols-[64px_28px_minmax(0,1fr)_110px] items-center gap-3 px-[18px] py-1.5 text-[13px] hover:bg-muted/30", open && "bg-muted/30")}>
        <span className="font-mono text-[11.5px] text-muted-foreground">{time}</span>
        <Who actor={e.actor} />
        <span className="min-w-0">
          <span className={cn(!isPerson(e.actor) && "text-secondary-foreground")}>{actorLabel(e.actor)}</span>{" "}
          {single || sum.sameThing ? (
            <>
              {verbOf(e)} <Thing e={e} names={names} alive={alive} />
              {tail && <> {tail}</>}
              {!single && <span className="text-muted-foreground"> {sum.times === 2 ? "twice" : `${sum.times} times`}</span>}
              {change && <span className="text-muted-foreground">: {change}</span>}
            </>
          ) : (
            <>
              {sum.verb}: <span className="font-mono text-[12.5px]">{sum.things.join(", ")}</span>
              {sum.more > 0 && <span className="text-muted-foreground"> and {sum.more} more</span>}
            </>
          )}
        </span>
        <button type="button" onClick={onToggle} aria-expanded={open} className={cn("text-right text-xs", open ? "text-foreground" : "text-muted-foreground hover:text-foreground")}>
          {open ? "hide" : single ? "details ›" : `${g.events.length} changes ›`}
        </button>
      </div>
      {open && (single ? (
        <Detail e={e} entries={entries} />
      ) : (
        <div className="mb-2 ml-[104px] mr-[18px] rounded-lg border border-border bg-background px-3.5 py-1.5">
          {g.events.map((x) => {
            const c = changeLine(x, previousOf(x, entries));
            return (
              <div key={x.id} className="grid grid-cols-[56px_minmax(0,1fr)] gap-3 border-t border-border py-1.5 text-[12.5px] first:border-t-0">
                <span className="font-mono text-[11px] text-muted-foreground">{new Date(x.ts).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" })}</span>
                <span className="min-w-0 truncate"><Thing e={x} names={names} alive={alive} />{c && <span className="text-muted-foreground"> · {c}</span>}</span>
              </div>
            );
          })}
        </div>
      ))}
    </>
  );
}

export function AuditLog({ entries: initial, filters, names, alive: aliveIds, total }: { entries: AuditEntry[]; filters: AuditFilters; names: Record<string, string>; alive: string[]; total: number | null }) {
  const router = useRouter();
  const pathname = usePathname();
  const [extra, setExtra] = useState<AuditEntry[]>([]);
  const [done, setDone] = useState(initial.length < PAGE);
  const [open, setOpen] = useState<Set<number>>(new Set());
  const [query, setQuery] = useState(filters.q);
  const [loading, start] = useTransition();
  const alive = useMemo(() => new Set(aliveIds), [aliveIds]);
  const entries = useMemo(() => [...initial, ...extra].filter((e) => matchesKind(e, filters.kind)), [initial, extra, filters.kind]);
  const groups = useMemo(() => groupBursts(entries), [entries]);

  useEffect(() => { setExtra([]); setDone(initial.length < PAGE); setOpen(new Set()); }, [initial]);

  const go = (patch: Partial<AuditFilters>) => router.replace(`${pathname}${paramsFor({ ...filters, ...patch })}`, { scroll: false });
  useEffect(() => {
    if (query === filters.q) return;
    const t = setTimeout(() => go({ q: query }), 400);
    return () => clearTimeout(t);
  }, [query]); // eslint-disable-line react-hooks/exhaustive-deps

  function older() {
    const last = [...initial, ...extra].at(-1);
    if (!last) return;
    start(async () => {
      const q = queryFor(filters);
      const p = new URLSearchParams();
      Object.entries({ actor: q.actor, entity_type: q.entityType, entity_id: q.entityId, since: q.since, q: q.q }).forEach(([k, v]) => { if (v) p.set(k, v); });
      p.set("before_id", String(last.id));
      p.set("limit", String(PAGE));
      const res = await fetch(`/api/live/audit?${p}`);
      const rows = res.ok ? ((await res.json()) as AuditEntry[]) : [];
      setExtra((x) => [...x, ...rows]);
      if (rows.length < PAGE) setDone(true);
    });
  }

  function exportCsv() {
    const blob = new Blob([toCsv(entries, names)], { type: "text/csv" });
    const a = document.createElement("a");
    a.href = URL.createObjectURL(blob);
    a.download = `audit_${new Date().toISOString().slice(0, 10)}.csv`;
    a.click();
    URL.revokeObjectURL(a.href);
  }

  // Tiles, over what's in view.
  const days = new Map<string, number>();
  for (const e of entries) days.set(dayLabel(e.ts), (days.get(dayLabel(e.ts)) ?? 0) + 1);
  const busiest = [...days.entries()].sort((a, b) => b[1] - a[1])[0];
  const actors = new Map<string, number>();
  for (const e of entries) actors.set(e.actor, (actors.get(e.actor) ?? 0) + 1);
  const topActors = [...actors.entries()].sort((a, b) => b[1] - a[1]);
  const kinds = new Map<AuditKind, number>();
  for (const e of entries) kinds.set(kindOf(e.entity_type), (kinds.get(kindOf(e.entity_type)) ?? 0) + 1);
  const topKind = [...kinds.entries()].sort((a, b) => b[1] - a[1])[0];
  const things = new Map<string, number>();
  for (const e of entries) if (topKind && kindOf(e.entity_type) === topKind[0] && e.entity_type !== "settings") things.set(e.entity_id, (things.get(e.entity_id) ?? 0) + 1);
  const topThing = [...things.entries()].sort((a, b) => b[1] - a[1])[0];
  const deletions = entries.filter((e) => e.action.startsWith("delete_"));
  const delKinds = new Map<string, number>();
  for (const e of deletions) delKinds.set(e.entity_type, (delKinds.get(e.entity_type) ?? 0) + 1);
  const pct = (n: number) => `${Math.round((n / Math.max(1, entries.length)) * 100)}%`;
  const whoOptions = [...new Set([filters.who, ...topActors.map(([a]) => a)])].filter(Boolean);
  const [entityType, entityId] = filters.entity.includes(":") ? filters.entity.split(":", 2) : ["", ""];
  const rangeWord = filters.range === "all" ? "in view" : `in ${RANGE_LABEL[filters.range].toLowerCase()}`;

  let lastDay = "";
  return (
    <div className="mx-auto flex max-w-[1400px] flex-col gap-[18px]">
      <div className="flex flex-wrap items-end justify-between gap-4">
        <div className="flex min-w-0 flex-col gap-1.5">
          <h1 className="text-[26px] font-semibold tracking-tight">Audit</h1>
          <p className="text-[13px] text-secondary-foreground">Every change made through the dashboard or the admin API · {entries.length}{done ? "" : "+"} {rangeWord}{total != null ? ` · ${total} in all` : ""}</p>
        </div>
        <Button type="button" variant="outline" size="sm" className="h-9" disabled={entries.length === 0} onClick={exportCsv}><Download className="h-4 w-4" />Export</Button>
      </div>

      <div className="grid grid-cols-2 gap-3 xl:grid-cols-4">
        <Tile label={`Changes · ${RANGE_LABEL[filters.range].toLowerCase()}`} value={`${entries.length}${done ? "" : "+"}`} detail={busiest && days.size > 1 ? `${busiest[1]} of them ${busiest[0] === "Today" || busiest[0] === "Yesterday" ? busiest[0].toLowerCase() : `on ${busiest[0]}`}` : null} />
        <Tile label="People" value={actors.size} detail={topActors.slice(0, 2).map(([a, n]) => `${actorLabel(a)} ${pct(n)}`).join(" · ") || null} />
        <Tile label="Most changed" value={topKind ? KIND_LABEL[topKind[0]] : "—"} detail={topKind ? `${topKind[1]} changes${topThing ? ` · ${names[topThing[0]] ?? "one"} the most, ${topThing[1]}` : ""}` : null} />
        <Tile label="Deletions" value={deletions.length} detail={deletions.length ? [...delKinds.entries()].map(([k, n]) => `${n} ${k.replace(/_/g, " ")}${n === 1 ? "" : "s"}`).join(", ") : "none"} />
      </div>

      <div className="flex flex-wrap items-center gap-2">
        <label className="flex h-9 min-w-[240px] max-w-[360px] flex-1 items-center gap-2 rounded-lg border border-border bg-background px-3 text-[13px] focus-within:border-muted-foreground">
          <Search className="h-4 w-4 shrink-0 text-muted-foreground" aria-hidden />
          <input value={query} onChange={(e) => setQuery(e.target.value)} aria-label="Search the audit log" placeholder="A name, id, or what changed" className="min-w-0 flex-1 bg-transparent outline-none placeholder:text-muted-foreground" />
        </label>
        <Segmented<AuditRange> label="Time" value={filters.range} onChange={(range) => go({ range })} options={(["24h", "7d", "30d", "all"] as const).map((r) => ({ value: r, label: RANGE_LABEL[r] }))} />
        <Select aria-label="Who" value={filters.who} onValueChange={(who) => go({ who })} className={cn("h-9 w-auto min-w-[9rem] text-[12.5px]", filters.who && "border-foreground")} options={[{ value: "", label: "Anyone" }, ...whoOptions.map((a) => ({ value: a, label: actorLabel(a) }))]} />
        <Select aria-label="Kind" value={filters.kind} onValueChange={(kind) => go({ kind: kind as AuditKind | "" })} className={cn("h-9 w-auto min-w-[9rem] text-[12.5px]", filters.kind && "border-foreground")} options={[{ value: "", label: "Anything" }, ...(Object.keys(KIND_LABEL) as AuditKind[]).map((k) => ({ value: k, label: KIND_LABEL[k] }))]} />
        {filters.entity && (
          <button type="button" onClick={() => go({ entity: "", range: "7d" })} className="inline-flex h-9 items-center gap-2 rounded-lg border border-foreground px-3 text-[12.5px]">
            Only {entityType.replace(/_/g, " ")} <span className="font-mono">{names[entityId] ?? entityId.slice(0, 8)}</span><X className="h-3.5 w-3.5" aria-label="Show everything" />
          </button>
        )}
      </div>

      <section aria-label="Changes" className="overflow-hidden rounded-xl border border-border bg-card">
        {groups.length === 0 && <p className="px-[18px] py-10 text-center text-[13px] text-muted-foreground">{filters.q || filters.who || filters.kind || filters.entity ? "No change matches these filters." : `No changes ${rangeWord}.`}</p>}
        {groups.map((g) => {
          const day = dayLabel(g.events[0].ts);
          const header = day !== lastDay;
          lastDay = day;
          const id = g.events[0].id;
          return (
            <Fragment key={id}>
              {header && <div className="border-t border-border px-[18px] pb-1.5 pt-3.5 text-[11px] font-semibold uppercase tracking-[0.07em] text-muted-foreground first:border-t-0">{day}</div>}
              <GroupLine g={g} names={names} alive={alive} entries={entries} open={open.has(id)} onToggle={() => setOpen((o) => { const n = new Set(o); if (n.has(id)) n.delete(id); else n.add(id); return n; })} />
            </Fragment>
          );
        })}
        <div className="flex flex-wrap justify-between gap-2 border-t border-border px-[18px] py-3 text-[12.5px] text-muted-foreground">
          <span>
            {done ? (filters.range === "all" ? "That's everything." : <>That&apos;s {RANGE_LABEL[filters.range].toLowerCase()}. <button type="button" onClick={() => go({ range: "all" })} className="text-secondary-foreground underline underline-offset-[3px] hover:text-foreground">Show all</button></>) : <button type="button" disabled={loading} onClick={older} className="text-secondary-foreground underline underline-offset-[3px] hover:text-foreground">{loading ? "Loading…" : "Load older"}</button>}
          </span>
          <span>Dashed avatar: a shared token, not a person</span>
        </div>
      </section>
    </div>
  );
}
