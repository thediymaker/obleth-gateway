"use client";

import { Fragment, useMemo, useState, type ReactNode } from "react";
import {
  Activity,
  Box,
  ChevronDown,
  ChevronLeft,
  ChevronRight,
  ChevronsLeft,
  ChevronsRight,
  Clock3,
  FilterX,
  Search,
  UserRound,
  Users,
} from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Select } from "@/components/ui/select";
import type { AuditEntry } from "@/lib/obleth";
import { cn, formatNumber } from "@/lib/utils";

const PAGE_SIZES = [10, 25, 50, 100] as const;
const DEFAULT_PAGE_SIZE = 25;
const MAX_SUMMARY_FIELDS = 3;

interface Filters {
  query: string;
  actor: string;
  action: string;
  entityType: string;
}

interface TenantOption {
  id: string;
  name: string;
}

const DEFAULT_FILTERS: Filters = {
  query: "",
  actor: "",
  action: "",
  entityType: "",
};

export function AuditTable({ entries, tenants = [] }: { entries: AuditEntry[]; tenants?: TenantOption[] }) {
  const [filters, setFilters] = useState<Filters>(DEFAULT_FILTERS);
  const [pageSize, setPageSize] = useState<number>(DEFAULT_PAGE_SIZE);
  const [page, setPage] = useState(0);
  const [expandedId, setExpandedId] = useState<number | null>(null);

  const tenantNames = useMemo(() => new Map(tenants.map((tenant) => [tenant.id, tenant.name])), [tenants]);
  const actors = useMemo(() => uniqueSorted(entries.map((e) => e.actor)), [entries]);
  const actions = useMemo(() => uniqueSorted(entries.map((e) => e.action)), [entries]);
  const entityTypes = useMemo(() => uniqueSorted(entries.map((e) => e.entity_type)), [entries]);

  const filtered = useMemo(() => {
    const q = filters.query.trim().toLowerCase();
    return entries.filter((entry) => {
      if (filters.actor && entry.actor !== filters.actor) return false;
      if (filters.action && entry.action !== filters.action) return false;
      if (filters.entityType && entry.entity_type !== filters.entityType) return false;
      if (!q) return true;
      return searchText(entry, tenantNames).includes(q);
    });
  }, [entries, filters, tenantNames]);

  const stats = useMemo(() => buildStats(entries, filtered), [entries, filtered]);
  const pageCount = Math.max(1, Math.ceil(filtered.length / pageSize));
  const safePage = Math.min(page, pageCount - 1);
  const offset = safePage * pageSize;
  const rows = filtered.slice(offset, offset + pageSize);
  const filtersActive = !filtersEqual(filters, DEFAULT_FILTERS) || pageSize !== DEFAULT_PAGE_SIZE;

  function patchFilters(patch: Partial<Filters>) {
    setFilters((current) => ({ ...current, ...patch }));
    setPage(0);
    setExpandedId(null);
  }

  function resetFilters() {
    setFilters(DEFAULT_FILTERS);
    setPageSize(DEFAULT_PAGE_SIZE);
    setPage(0);
    setExpandedId(null);
  }

  function toggleExpanded(id: number) {
    setExpandedId((current) => (current === id ? null : id));
  }

  function selectPageSize(value: string) {
    setPageSize(Number(value));
    setPage(0);
    setExpandedId(null);
  }

  return (
    <div className="space-y-4">
      <AuditStats stats={stats} />

      <Card className="overflow-hidden rounded-md border-border/80">
        <div className="grid gap-2.5 border-b border-border bg-card/80 px-3 py-3">
          <div className="grid gap-2 md:grid-cols-[minmax(14rem,1fr)_8.5rem_auto]">
            <div className="relative min-w-0">
              <Search className="pointer-events-none absolute left-2.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" />
              <Input
                type="search"
                value={filters.query}
                onChange={(e) => patchFilters({ query: e.target.value })}
                placeholder="Search actor, action, target, or detail"
                aria-label="Search audit events"
                className="h-8 w-full pl-8 text-xs"
              />
            </div>
            <Select
              value={String(pageSize)}
              onChange={(e) => selectPageSize(e.target.value)}
              aria-label="Rows per page"
              className="h-8 w-full text-xs"
            >
              {PAGE_SIZES.map((n) => (
                <option key={n} value={n}>
                  {n} / page
                </option>
              ))}
            </Select>
            {filtersActive && (
              <Button type="button" variant="secondary" size="sm" onClick={resetFilters} className="h-8">
                <FilterX className="h-3.5 w-3.5" />
                Reset
              </Button>
            )}
          </div>

          <div className="grid gap-2 sm:grid-cols-3">
            <Select
              value={filters.actor}
              onChange={(e) => patchFilters({ actor: e.target.value })}
              aria-label="Filter audit events by actor"
              className="h-8 w-full text-xs"
            >
              <option value="">All actors</option>
              {actors.map((actor) => (
                <option key={actor} value={actor}>
                  {actor}
                </option>
              ))}
            </Select>
            <Select
              value={filters.action}
              onChange={(e) => patchFilters({ action: e.target.value })}
              aria-label="Filter audit events by action"
              className="h-8 w-full text-xs"
            >
              <option value="">All actions</option>
              {actions.map((action) => (
                <option key={action} value={action}>
                  {labelize(action)}
                </option>
              ))}
            </Select>
            <Select
              value={filters.entityType}
              onChange={(e) => patchFilters({ entityType: e.target.value })}
              aria-label="Filter audit events by entity type"
              className="h-8 w-full text-xs"
            >
              <option value="">All targets</option>
              {entityTypes.map((entityType) => (
                <option key={entityType} value={entityType}>
                  {labelize(entityType)}
                </option>
              ))}
            </Select>
          </div>

          <AuditPager
            filteredCount={filtered.length}
            totalCount={entries.length}
            page={safePage}
            pageCount={pageCount}
            pageSize={pageSize}
            onPage={setPage}
          />
        </div>

        <CardContent className="p-0">
          <div className="hidden overflow-x-auto md:block">
            <table className="w-full min-w-[760px] text-sm">
              <thead>
                <tr className="border-b border-border text-left text-xs text-muted-foreground">
                  <th className="w-36 px-3 py-2 font-medium">Time</th>
                  <th className="px-3 py-2 font-medium">Actor</th>
                  <th className="px-3 py-2 font-medium">Action</th>
                  <th className="px-3 py-2 font-medium">Target</th>
                  <th className="px-3 py-2 font-medium">Change</th>
                  <th className="w-10 px-3 py-2 font-medium" />
                </tr>
              </thead>
              <tbody>
                {rows.map((entry) => {
                  const expanded = expandedId === entry.id;
                  return (
                    <Fragment key={entry.id}>
                      <AuditRow
                        entry={entry}
                        expanded={expanded}
                        onExpand={() => toggleExpanded(entry.id)}
                        onActor={() => patchFilters({ actor: entry.actor })}
                        onAction={() => patchFilters({ action: entry.action })}
                        onEntityType={() => patchFilters({ entityType: entry.entity_type })}
                        tenantNames={tenantNames}
                      />
                      {expanded && (
                        <tr>
                          <td colSpan={6} className="border-b border-border/60 bg-muted/10 p-0">
                            <AuditDetail entry={entry} tenantNames={tenantNames} />
                          </td>
                        </tr>
                      )}
                    </Fragment>
                  );
                })}
                {rows.length === 0 && (
                  <tr>
                    <td colSpan={6} className="px-4 py-16 text-center text-muted-foreground">
                      {entries.length === 0 ? "No audit events yet." : "No audit events match the current filters."}
                    </td>
                  </tr>
                )}
              </tbody>
            </table>
          </div>

          <div className="divide-y divide-border/60 md:hidden">
            {rows.map((entry) => {
              const expanded = expandedId === entry.id;
              return (
                <Fragment key={entry.id}>
                  <AuditMobileRow
                    entry={entry}
                    expanded={expanded}
                    onExpand={() => toggleExpanded(entry.id)}
                    onActor={() => patchFilters({ actor: entry.actor })}
                    onAction={() => patchFilters({ action: entry.action })}
                    onEntityType={() => patchFilters({ entityType: entry.entity_type })}
                    tenantNames={tenantNames}
                  />
                  {expanded && <AuditDetail entry={entry} tenantNames={tenantNames} />}
                </Fragment>
              );
            })}
            {rows.length === 0 && (
              <div className="px-4 py-14 text-center text-sm text-muted-foreground">
                {entries.length === 0 ? "No audit events yet." : "No audit events match the current filters."}
              </div>
            )}
          </div>

          {filtered.length > 0 && (
            <div className="border-t border-border px-3 py-3">
              <AuditPager
                filteredCount={filtered.length}
                totalCount={entries.length}
                page={safePage}
                pageCount={pageCount}
                pageSize={pageSize}
                onPage={setPage}
              />
            </div>
          )}
        </CardContent>
      </Card>
    </div>
  );
}

function AuditStats({ stats }: { stats: AuditStatsView }) {
  return (
    <div className="grid grid-cols-2 gap-3 lg:grid-cols-4">
      <StatCard
        icon={Activity}
        label="Events"
        value={formatNumber(stats.filteredEvents)}
        hint={stats.filteredEvents === stats.totalEvents ? "loaded rows" : `${formatNumber(stats.totalEvents)} loaded`}
      />
      <StatCard
        icon={Users}
        label="Actors"
        value={formatNumber(stats.actorCount)}
        hint={stats.topActor ? `${stats.topActor.name} (${formatNumber(stats.topActor.count)})` : "no actors"}
      />
      <StatCard
        icon={Box}
        label="Targets"
        value={formatNumber(stats.entityTypeCount)}
        hint={stats.topEntity ? `${labelize(stats.topEntity.name)} (${formatNumber(stats.topEntity.count)})` : "no targets"}
      />
      <StatCard
        icon={Clock3}
        label="Latest"
        value={stats.latest ? shortDate(stats.latest.ts) : "-"}
        hint={stats.latest ? labelize(stats.latest.action) : "no events"}
      />
    </div>
  );
}

function StatCard({
  icon: Icon,
  label,
  value,
  hint,
}: {
  icon: typeof Activity;
  label: string;
  value: string;
  hint: string;
}) {
  return (
    <div className="rounded-lg border border-border bg-card p-4">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <p className="text-xs text-muted-foreground">{label}</p>
          <p className="mt-1 truncate text-2xl font-semibold tabular-nums" title={value}>
            {value}
          </p>
          <p className="mt-0.5 truncate text-[11px] text-muted-foreground" title={hint}>
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

function AuditRow({
  entry,
  expanded,
  onExpand,
  onActor,
  onAction,
  onEntityType,
  tenantNames,
}: {
  entry: AuditEntry;
  expanded: boolean;
  onExpand: () => void;
  onActor: () => void;
  onAction: () => void;
  onEntityType: () => void;
  tenantNames: Map<string, string>;
}) {
  return (
    <tr
      onClick={onExpand}
      className={cn(
        "cursor-pointer select-none border-b border-border/60 transition-colors hover:bg-muted/20",
        expanded && "border-b-0 bg-muted/10",
      )}
    >
      <td className="px-3 py-2 tabular-nums text-muted-foreground">
        <TimeStamp iso={entry.ts} />
      </td>
      <td className="px-3 py-2">
        <FilterChip onClick={onActor} title={`Filter to ${entry.actor}`}>
          <UserRound className="h-3 w-3" aria-hidden />
          <span className="max-w-[12rem] truncate">{entry.actor}</span>
        </FilterChip>
      </td>
      <td className="px-3 py-2">
        <FilterChip onClick={onAction} title={`Filter to ${labelize(entry.action)}`}>
          {labelize(entry.action)}
        </FilterChip>
      </td>
      <td className="px-3 py-2">
        <button
          type="button"
          onClick={(event) => {
            event.stopPropagation();
            onEntityType();
          }}
          className="group flex min-w-0 flex-col items-start text-left"
          title={`${entry.entity_type} ${entry.entity_id}`}
        >
          <span className="truncate text-xs text-foreground group-hover:underline">{labelize(entry.entity_type)}</span>
          <span className="font-mono text-[11px] text-muted-foreground">{shortId(entry.entity_id)}</span>
        </button>
      </td>
      <td className="max-w-[24rem] px-3 py-2 text-xs text-muted-foreground">
        <span className="line-clamp-2">{detailSummary(entry.detail, tenantNames)}</span>
      </td>
      <td className="px-3 py-2 text-right text-muted-foreground">
        <ChevronDown className={cn("h-4 w-4 transition-transform", expanded && "rotate-180 text-foreground")} />
      </td>
    </tr>
  );
}

function AuditMobileRow({
  entry,
  expanded,
  onExpand,
  onActor,
  onAction,
  onEntityType,
  tenantNames,
}: {
  entry: AuditEntry;
  expanded: boolean;
  onExpand: () => void;
  onActor: () => void;
  onAction: () => void;
  onEntityType: () => void;
  tenantNames: Map<string, string>;
}) {
  return (
    <div className={cn("bg-card/40 transition-colors", expanded && "bg-muted/15")}>
      <button type="button" onClick={onExpand} className="block w-full px-3 py-3 text-left" aria-expanded={expanded}>
        <div className="flex items-start justify-between gap-3">
          <div className="min-w-0">
            <div className="flex flex-wrap items-center gap-1.5">
              <Badge className="max-w-full bg-background/60 text-foreground">
                <span className="truncate">{labelize(entry.action)}</span>
              </Badge>
              <Badge className="bg-background/60 text-muted-foreground">{labelize(entry.entity_type)}</Badge>
            </div>
            <p className="mt-2 truncate font-mono text-xs text-muted-foreground" title={entry.entity_id}>
              {shortId(entry.entity_id)}
            </p>
          </div>
          <div className="flex shrink-0 items-start gap-2">
            <TimeStamp iso={entry.ts} align="right" />
            <ChevronDown className={cn("mt-0.5 h-4 w-4 text-muted-foreground transition-transform", expanded && "rotate-180 text-foreground")} />
          </div>
        </div>

        <p className="mt-3 line-clamp-2 text-xs leading-relaxed text-muted-foreground">
          {detailSummary(entry.detail, tenantNames)}
        </p>
      </button>
      <div className="flex flex-wrap gap-1.5 px-3 pb-3">
        <FilterChip onClick={onActor} title={`Filter to ${entry.actor}`}>
          <UserRound className="h-3 w-3" aria-hidden />
          <span className="max-w-[14rem] truncate">{entry.actor}</span>
        </FilterChip>
        <FilterChip onClick={onAction} title={`Filter to ${labelize(entry.action)}`}>
          {labelize(entry.action)}
        </FilterChip>
        <FilterChip onClick={onEntityType} title={`Filter to ${labelize(entry.entity_type)}`}>
          {labelize(entry.entity_type)}
        </FilterChip>
      </div>
    </div>
  );
}

function AuditDetail({ entry, tenantNames }: { entry: AuditEntry; tenantNames: Map<string, string> }) {
  const tenant = tenantLabelFromDetail(entry.detail, tenantNames);

  return (
    <div className="space-y-3 px-3 py-3 md:px-4">
      <div className="grid gap-2 text-xs sm:grid-cols-2 lg:grid-cols-4">
        <Field label="Actor" value={entry.actor} />
        <Field label="Action" value={labelize(entry.action)} mono />
        <Field label="Target" value={labelize(entry.entity_type)} />
        {tenant && <Field label="Tenant" value={tenant} />}
        <Field label="Event ID" value={String(entry.id)} mono />
        <div className="sm:col-span-2 lg:col-span-4">
          <Field label="Entity ID" value={entry.entity_id} mono />
        </div>
      </div>
      <pre className="max-h-72 overflow-auto rounded-md border border-border bg-background/80 px-3 py-2 font-mono text-[11px] leading-relaxed text-foreground/90">
        {safeJson(entry.detail)}
      </pre>
    </div>
  );
}

function AuditPager({
  filteredCount,
  totalCount,
  page,
  pageCount,
  pageSize,
  onPage,
}: {
  filteredCount: number;
  totalCount: number;
  page: number;
  pageCount: number;
  pageSize: number;
  onPage: (page: number) => void;
}) {
  const start = filteredCount === 0 ? 0 : page * pageSize + 1;
  const end = Math.min((page + 1) * pageSize, filteredCount);
  return (
    <div className="flex flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
      <span className="text-xs tabular-nums text-muted-foreground">
        {formatNumber(start)}-{formatNumber(end)} of {formatNumber(filteredCount)}
        {filteredCount !== totalCount ? ` filtered from ${formatNumber(totalCount)}` : ""}
      </span>
      <div className="flex items-center gap-1.5">
        <IconPagerButton label="First page" disabled={page === 0} onClick={() => onPage(0)}>
          <ChevronsLeft className="h-3.5 w-3.5" />
        </IconPagerButton>
        <Button type="button" variant="outline" size="sm" onClick={() => onPage(page - 1)} disabled={page === 0}>
          <ChevronLeft className="h-3.5 w-3.5" />
          Newer
        </Button>
        <span className="hidden min-w-20 text-center text-xs tabular-nums text-muted-foreground sm:inline">
          {formatNumber(page + 1)} / {formatNumber(pageCount)}
        </span>
        <Button
          type="button"
          variant="outline"
          size="sm"
          onClick={() => onPage(page + 1)}
          disabled={page >= pageCount - 1}
        >
          Older
          <ChevronRight className="h-3.5 w-3.5" />
        </Button>
        <IconPagerButton label="Last page" disabled={page >= pageCount - 1} onClick={() => onPage(pageCount - 1)}>
          <ChevronsRight className="h-3.5 w-3.5" />
        </IconPagerButton>
      </div>
    </div>
  );
}

function IconPagerButton({
  label,
  disabled,
  onClick,
  children,
}: {
  label: string;
  disabled: boolean;
  onClick: () => void;
  children: ReactNode;
}) {
  return (
    <Button type="button" variant="outline" size="icon" className="h-8 w-8" aria-label={label} title={label} onClick={onClick} disabled={disabled}>
      {children}
    </Button>
  );
}

function FilterChip({
  onClick,
  title,
  children,
}: {
  onClick: () => void;
  title: string;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      onClick={(event) => {
        event.stopPropagation();
        onClick();
      }}
      className="inline-flex max-w-full items-center gap-1.5 rounded-md border border-border bg-background/60 px-2 py-0.5 text-xs text-muted-foreground transition-colors hover:border-muted-foreground/40 hover:text-foreground"
      title={title}
    >
      {children}
    </button>
  );
}

function Field({ label, value, mono }: { label: string; value: string; mono?: boolean }) {
  return (
    <div className="min-w-0 rounded-md border border-border/60 bg-background/35 px-3 py-2">
      <p className="text-[11px] text-muted-foreground">{label}</p>
      <p className={cn("mt-0.5 truncate text-xs text-foreground", mono && "font-mono")} title={value}>
        {value || "-"}
      </p>
    </div>
  );
}

function TimeStamp({ iso, align = "left" }: { iso: string; align?: "left" | "right" }) {
  const { date, time } = timeParts(iso);
  return (
    <span className={cn("flex flex-col text-xs leading-tight", align === "right" ? "items-end text-right" : "items-start")}>
      <span>{date}</span>
      <span>{time}</span>
    </span>
  );
}

interface AuditStatsView {
  totalEvents: number;
  filteredEvents: number;
  actorCount: number;
  entityTypeCount: number;
  topActor: Counted | null;
  topEntity: Counted | null;
  latest: AuditEntry | null;
}

interface Counted {
  name: string;
  count: number;
}

function buildStats(entries: AuditEntry[], filtered: AuditEntry[]): AuditStatsView {
  const actorCounts = counts(filtered.map((entry) => entry.actor));
  const entityCounts = counts(filtered.map((entry) => entry.entity_type));
  return {
    totalEvents: entries.length,
    filteredEvents: filtered.length,
    actorCount: actorCounts.length,
    entityTypeCount: entityCounts.length,
    topActor: actorCounts[0] ?? null,
    topEntity: entityCounts[0] ?? null,
    latest: filtered[0] ?? null,
  };
}

function counts(values: string[]): Counted[] {
  const map = new Map<string, number>();
  values.forEach((value) => map.set(value, (map.get(value) ?? 0) + 1));
  return [...map.entries()]
    .map(([name, count]) => ({ name, count }))
    .sort((a, b) => b.count - a.count || a.name.localeCompare(b.name));
}

function uniqueSorted(values: string[]): string[] {
  return [...new Set(values.filter(Boolean))].sort((a, b) => a.localeCompare(b));
}

function filtersEqual(a: Filters, b: Filters): boolean {
  return a.query === b.query && a.actor === b.actor && a.action === b.action && a.entityType === b.entityType;
}

function searchText(entry: AuditEntry, tenantNames: Map<string, string>): string {
  return [
    entry.actor,
    entry.action,
    entry.entity_type,
    entry.entity_id,
    tenantLabelFromDetail(entry.detail, tenantNames),
    detailSummary(entry.detail, tenantNames),
    safeJson(entry.detail),
  ]
    .join(" ")
    .toLowerCase();
}

function detailSummary(detail: unknown, tenantNames: Map<string, string>): string {
  if (!detail || typeof detail !== "object" || Array.isArray(detail)) {
    return String(detail ?? "No detail recorded.");
  }

  const record = detail as Record<string, unknown>;
  const preferred = [
    "name",
    "model_name",
    "tenant",
    "tenant_name",
    "key_name",
    "upstream_url",
    "api_base",
    "status",
    "enabled",
    "disabled",
    "weight",
    "tokens_per_minute",
    "max_in_flight",
    "budget_period",
    "partition",
    "state",
  ];

  const seen = new Set<string>();
  const fields: string[] = [];
  const tenant = tenantLabelFromRecord(record, tenantNames);
  if (tenant) {
    seen.add("tenant_id");
    seen.add("tenant_name");
    fields.push(`Tenant: ${tenant}`);
  }

  for (const key of preferred) {
    if (key in record) {
      seen.add(key);
      fields.push(`${labelize(key)}: ${formatValue(record[key])}`);
    }
    if (fields.length >= MAX_SUMMARY_FIELDS) break;
  }

  if (fields.length < MAX_SUMMARY_FIELDS) {
    for (const [key, value] of Object.entries(record)) {
      if (seen.has(key) || value == null || typeof value === "object") continue;
      fields.push(`${labelize(key)}: ${formatValue(value)}`);
      if (fields.length >= MAX_SUMMARY_FIELDS) break;
    }
  }

  return fields.length > 0 ? fields.join(" / ") : "Structured detail available.";
}

function tenantLabelFromDetail(detail: unknown, tenantNames: Map<string, string>): string | null {
  if (!detail || typeof detail !== "object" || Array.isArray(detail)) return null;
  return tenantLabelFromRecord(detail as Record<string, unknown>, tenantNames);
}

function tenantLabelFromRecord(record: Record<string, unknown>, tenantNames: Map<string, string>): string | null {
  if (typeof record.tenant_name === "string" && record.tenant_name.trim()) {
    return record.tenant_name.trim();
  }
  if (typeof record.tenant_id !== "string" || !record.tenant_id.trim()) return null;
  const id = record.tenant_id.trim();
  return tenantNames.get(id) ?? `Unknown tenant (${shortId(id)})`;
}

function formatValue(value: unknown): string {
  if (typeof value === "boolean") return value ? "yes" : "no";
  if (typeof value === "number") return formatNumber(value);
  if (typeof value === "string") return value || "-";
  if (value == null) return "-";
  return JSON.stringify(value);
}

function safeJson(detail: unknown): string {
  try {
    return JSON.stringify(detail, null, 2);
  } catch {
    return String(detail);
  }
}

function labelize(value: string): string {
  return value
    .replace(/[_-]+/g, " ")
    .replace(/\s+/g, " ")
    .trim()
    .replace(/\b\w/g, (char) => char.toUpperCase());
}

function shortId(id: string): string {
  if (!id) return "-";
  return id.length > 16 ? `${id.slice(0, 8)}...${id.slice(-4)}` : id;
}

function shortDate(iso: string): string {
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return "-";
  return date.toLocaleString([], { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });
}

function timeParts(iso: string): { date: string; time: string } {
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return { date: "-", time: "" };
  return {
    date: date.toLocaleDateString([], {
      month: "2-digit",
      day: "2-digit",
      year: "2-digit",
    }),
    time: date.toLocaleTimeString([], {
      hour: "2-digit",
      minute: "2-digit",
      second: "2-digit",
    }),
  };
}
