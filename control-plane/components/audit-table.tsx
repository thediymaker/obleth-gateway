"use client";

import { Fragment, useMemo, useState } from "react";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Select } from "@/components/ui/select";
import type { AuditEntry } from "@/lib/obleth";

const PAGE_SIZE = 50;

export function AuditTable({ entries }: { entries: AuditEntry[] }) {
  const [query, setQuery] = useState("");
  const [action, setAction] = useState("all");
  const [page, setPage] = useState(0);
  const [expanded, setExpanded] = useState<Set<number>>(new Set());

  const actions = useMemo(() => [...new Set(entries.map((e) => e.action))].sort(), [entries]);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    return entries.filter((e) => {
      if (action !== "all" && e.action !== action) return false;
      if (!q) return true;
      return (
        e.actor.toLowerCase().includes(q) ||
        e.action.toLowerCase().includes(q) ||
        e.entity_type.toLowerCase().includes(q) ||
        e.entity_id.toLowerCase().includes(q)
      );
    });
  }, [entries, query, action]);

  const pageCount = Math.max(1, Math.ceil(filtered.length / PAGE_SIZE));
  const safePage = Math.min(page, pageCount - 1);
  const rows = filtered.slice(safePage * PAGE_SIZE, safePage * PAGE_SIZE + PAGE_SIZE);

  const toggle = (id: number) =>
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });

  const resetPage = <T,>(setter: (v: T) => void) => (v: T) => {
    setter(v);
    setPage(0);
  };

  return (
    <Card>
      <CardHeader className="gap-3 sm:flex-row sm:items-center sm:justify-between">
        <div>
          <CardTitle>Events</CardTitle>
          <CardDescription>
            {filtered.length} of {entries.length} events
            {pageCount > 1 ? ` · page ${safePage + 1} of ${pageCount}` : ""}
          </CardDescription>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <Input
            type="search"
            value={query}
            onChange={(e) => resetPage(setQuery)(e.target.value)}
            placeholder="Search actor, action, entity..."
            aria-label="Search audit events"
            className="h-8 w-56 text-xs"
          />
          <Select
            value={action}
            onChange={(e) => resetPage(setAction)(e.target.value)}
            aria-label="Filter audit events by action"
            className="h-8 max-w-[12rem] text-xs"
          >
            <option value="all">All actions</option>
            {actions.map((a) => (
              <option key={a} value={a}>
                {a}
              </option>
            ))}
          </Select>
        </div>
      </CardHeader>
      <CardContent className="p-0">
        <div className="overflow-x-auto">
          <table className="w-full text-sm">
            <thead>
              <tr className="border-b border-border text-left text-xs text-muted-foreground">
                <th className="px-6 py-3 font-medium">Time</th>
                <th className="px-3 py-3 font-medium">Actor</th>
                <th className="px-3 py-3 font-medium">Action</th>
                <th className="px-3 py-3 font-medium">Entity</th>
                <th className="px-3 py-3 font-medium" />
              </tr>
            </thead>
            <tbody>
              {rows.map((e) => {
                const open = expanded.has(e.id);
                return (
                  <Fragment key={e.id}>
                    <tr
                      className="cursor-pointer border-b border-border/60 align-top hover:bg-muted/20"
                      onClick={() => toggle(e.id)}
                    >
                      <td className="whitespace-nowrap px-6 py-3 text-xs text-muted-foreground">
                        {new Date(e.ts).toLocaleString()}
                      </td>
                      <td className="px-3 py-3">{e.actor}</td>
                      <td className="px-3 py-3 font-mono text-xs">{e.action}</td>
                      <td className="px-3 py-3 text-muted-foreground">
                        <span className="font-mono text-xs">{e.entity_type}</span>
                        <span className="text-muted-foreground/60"> / {e.entity_id.slice(0, 8)}…</span>
                      </td>
                      <td className="px-3 py-3 text-right text-xs text-muted-foreground">{open ? "Hide" : "Detail"}</td>
                    </tr>
                    {open && (
                      <tr className="border-b border-border/60 bg-background/40">
                        <td colSpan={5} className="px-6 py-3">
                          <div className="mb-2 grid grid-cols-2 gap-x-6 gap-y-1 text-xs text-muted-foreground sm:grid-cols-4">
                            <Field label="Entity ID" value={e.entity_id} mono />
                            <Field label="Entity type" value={e.entity_type} />
                            <Field label="Actor" value={e.actor} />
                            <Field label="Event ID" value={String(e.id)} />
                          </div>
                          <pre className="overflow-x-auto rounded-md border border-border bg-background px-3 py-2 font-mono text-[11px] leading-relaxed text-foreground/90">
                            {safeJson(e.detail)}
                          </pre>
                        </td>
                      </tr>
                    )}
                  </Fragment>
                );
              })}
              {filtered.length === 0 && (
                <tr>
                  <td colSpan={5} className="px-6 py-10 text-center text-muted-foreground">
                    {entries.length === 0 ? "No audit entries yet." : "No events match your filters."}
                  </td>
                </tr>
              )}
            </tbody>
          </table>
        </div>
        {pageCount > 1 && (
          <div className="flex items-center justify-between border-t border-border px-6 py-3 text-xs text-muted-foreground">
            <span>
              Showing {safePage * PAGE_SIZE + 1}–{Math.min((safePage + 1) * PAGE_SIZE, filtered.length)} of {filtered.length}
            </span>
            <div className="flex items-center gap-1">
              <Button variant="ghost" size="sm" disabled={safePage === 0} onClick={() => setPage(safePage - 1)}>
                Previous
              </Button>
              <Button variant="ghost" size="sm" disabled={safePage >= pageCount - 1} onClick={() => setPage(safePage + 1)}>
                Next
              </Button>
            </div>
          </div>
        )}
      </CardContent>
    </Card>
  );
}

function Field({ label, value, mono }: { label: string; value: string; mono?: boolean }) {
  return (
    <div className="min-w-0">
      <span className="text-muted-foreground/60">{label}: </span>
      <span className={mono ? "break-all font-mono text-foreground/90" : "text-foreground/90"}>{value}</span>
    </div>
  );
}

function safeJson(detail: unknown): string {
  try {
    return JSON.stringify(detail, null, 2);
  } catch {
    return String(detail);
  }
}
