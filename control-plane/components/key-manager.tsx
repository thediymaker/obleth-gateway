"use client";

import { Fragment, useMemo, useState, useTransition, type ReactNode } from "react";
import {
  ArrowDown,
  ArrowUp,
  ArrowUpDown,
  Check,
  ChevronDown,
  ChevronRight,
  Copy,
  Info,
  KeyRound,
  Plus,
  RefreshCw,
  Save,
  Search,
  SlidersHorizontal,
  Trash2,
} from "lucide-react";
import {
  createKeyAction,
  deleteFilteredKeysAction,
  deleteKeyAction,
  deleteKeysAction,
  toggleKeyAction,
  toggleKeyTracingAction,
  updateKeyAction,
} from "@/app/actions";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from "@/components/ui/dialog";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Select } from "@/components/ui/select";
import { Tooltip, TooltipContent, TooltipProvider, TooltipTrigger } from "@/components/ui/tooltip";
import type { ApiKey, KeyUsageSummary, Tenant } from "@/lib/obleth";
import { cn, formatCurrency, formatNumber } from "@/lib/utils";
import { useRouter } from "next/navigation";

const PAGE_SIZE_OPTIONS = [50, 100, 250, 500] as const;

type StatusFilter = "all" | "active" | "disabled";
type BudgetFilter = "all" | "budgeted" | "unlimited";
type SortKey = "tenant" | "name" | "status" | "budget" | "lastUsed" | "requests" | "tokens" | "cost" | "created";
type SortDirection = "asc" | "desc";

interface KeyRow {
  key: ApiKey;
  usage?: KeyUsageSummary;
  tenantName: string;
  description: string;
}

interface RowMessage {
  saved?: boolean;
  error?: string;
}

interface SortState {
  key: SortKey;
  direction: SortDirection;
}

export function KeyManager({
  tenants,
  keys,
  keyUsage,
}: {
  tenants: Tenant[];
  keys: ApiKey[];
  keyUsage: KeyUsageSummary[];
}) {
  const [secret, setSecret] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
  const [pending, start] = useTransition();
  const [refreshing, startRefresh] = useTransition();
  const router = useRouter();

  const [createOpen, setCreateOpen] = useState(false);
  const [createError, setCreateError] = useState<string | null>(null);
  const [rowMessages, setRowMessages] = useState<Record<string, RowMessage>>({});

  const [query, setQuery] = useState("");
  const [tenantFilter, setTenantFilter] = useState("all");
  const [statusFilter, setStatusFilter] = useState<StatusFilter>("all");
  const [budgetFilter, setBudgetFilter] = useState<BudgetFilter>("all");
  const [page, setPage] = useState(0);
  const [pageSize, setPageSize] = useState<number>(100);
  const [selectedIds, setSelectedIds] = useState<Set<string>>(() => new Set());
  const [expandedId, setExpandedId] = useState<string | null>(null);
  const [sort, setSort] = useState<SortState>({ key: "lastUsed", direction: "desc" });

  const tenantNameMap = useMemo(() => Object.fromEntries(tenants.map((t) => [t.id, t.name])), [tenants]);
  const usageMap = useMemo(() => new Map(keyUsage.map((u) => [u.key_id, u])), [keyUsage]);

  const rows = useMemo<KeyRow[]>(
    () =>
      keys.map((key) => ({
        key,
        usage: usageMap.get(key.id),
        tenantName: tenantNameMap[key.tenant_id] ?? key.tenant_id.slice(0, 8),
        description: key.description ?? "",
      })),
    [keys, tenantNameMap, usageMap],
  );

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    return rows.filter(({ key, tenantName, description }) => {
      if (tenantFilter !== "all" && key.tenant_id !== tenantFilter) return false;
      if (statusFilter === "active" && key.disabled) return false;
      if (statusFilter === "disabled" && !key.disabled) return false;
      if (budgetFilter === "budgeted" && !hasKeyBudget(key)) return false;
      if (budgetFilter === "unlimited" && hasKeyBudget(key)) return false;
      if (!q) return true;
      return (
        key.key_prefix.toLowerCase().includes(q) ||
        key.name.toLowerCase().includes(q) ||
        description.toLowerCase().includes(q) ||
        tenantName.toLowerCase().includes(q)
      );
    });
  }, [rows, query, tenantFilter, statusFilter, budgetFilter]);

  const sorted = useMemo(() => {
    const direction = sort.direction === "asc" ? 1 : -1;
    return [...filtered].sort((a, b) => {
      const av = sortValue(a, sort.key);
      const bv = sortValue(b, sort.key);
      const primary = compareSortValues(av, bv) * direction;
      if (primary !== 0) return primary;
      return a.key.name.localeCompare(b.key.name);
    });
  }, [filtered, sort]);

  const pageCount = Math.max(1, Math.ceil(sorted.length / pageSize));
  const safePage = Math.min(page, pageCount - 1);
  const pageRows = sorted.slice(safePage * pageSize, safePage * pageSize + pageSize);
  const selectedCount = selectedIds.size;
  const pageSelected = pageRows.length > 0 && pageRows.every(({ key }) => selectedIds.has(key.id));
  const hasActiveFilter =
    query.trim() !== "" || tenantFilter !== "all" || statusFilter !== "all" || budgetFilter !== "all";
  const activeCount = keys.filter((k) => !k.disabled).length;
  const budgetedCount = keys.filter(hasKeyBudget).length;
  const recentTraffic = keyUsage.reduce((sum, u) => sum + Number(u.total_tokens ?? 0), 0);

  const onFilterChange = <T,>(setter: (v: T) => void) => (v: T) => {
    setter(v);
    setPage(0);
  };

  function changeSort(key: SortKey) {
    setSort((current) =>
      current.key === key
        ? { key, direction: current.direction === "asc" ? "desc" : "asc" }
        : { key, direction: defaultSortDirection(key) },
    );
  }

  async function copySecret() {
    if (!secret) return;
    await navigator.clipboard.writeText(secret);
    setCopied(true);
    window.setTimeout(() => setCopied(false), 1500);
  }

  function createKey(formData: FormData) {
    setCreateError(null);
    start(async () => {
      const result = await createKeyAction(formData);
      if (result.ok) {
        setSecret(result.secret ?? null);
        setCopied(false);
        setCreateOpen(false);
      } else {
        setCreateError(result.error);
      }
    });
  }

  function saveKey(formData: FormData) {
    const id = String(formData.get("id") ?? "");
    if (!id) return;
    setRowMessages((current) => ({ ...current, [id]: {} }));
    start(async () => {
      const result = await updateKeyAction(formData);
      setRowMessages((current) => ({
        ...current,
        [id]: result.ok ? { saved: true } : { error: result.error },
      }));
    });
  }

  function removeKey(key: ApiKey) {
    if (!window.confirm(`Delete API key "${key.name}"? This cannot be undone.`)) return;
    start(() => deleteKeyAction(key.id));
  }

  function toggleKeySelection(id: string, checked: boolean) {
    setSelectedIds((current) => {
      const next = new Set(current);
      if (checked) next.add(id);
      else next.delete(id);
      return next;
    });
  }

  function togglePageSelection() {
    setSelectedIds((current) => {
      const next = new Set(current);
      if (pageSelected) {
        for (const { key } of pageRows) next.delete(key.id);
      } else {
        for (const { key } of pageRows) next.add(key.id);
      }
      return next;
    });
  }

  function deleteSelectedKeys() {
    const ids = [...selectedIds];
    if (ids.length === 0) return;
    if (!window.confirm(`Delete ${ids.length} selected API keys? This cannot be undone.`)) return;
    start(async () => {
      const result = await deleteKeysAction(ids);
      setSelectedIds(new Set());
      if (result.failed > 0) {
        window.alert(`Deleted ${result.deleted} keys; ${result.failed} failed.`);
      }
    });
  }

  function deleteFilteredKeys() {
    if (!hasActiveFilter || sorted.length === 0) return;
    if (!window.confirm(`Delete ${sorted.length} filtered API keys? This cannot be undone.`)) return;
    start(async () => {
      const result = await deleteFilteredKeysAction({
        query,
        tenantId: tenantFilter,
        status: statusFilter,
        budget: budgetFilter,
      });
      setSelectedIds(new Set());
      if (result.failed > 0) {
        window.alert(`Deleted ${result.deleted} of ${result.matched} matched keys; ${result.failed} failed.`);
      }
    });
  }

  function refreshData() {
    startRefresh(() => {
      router.refresh();
    });
  }

  return (
    <TooltipProvider delayDuration={150}>
      <div className="space-y-5">
        {secret && (
          <Card className="border-foreground/25">
            <CardContent className="pt-6">
              <p className="mb-2 text-sm text-muted-foreground">Copy this key now. It is shown only once.</p>
              <code className="block break-all rounded-md border border-border bg-background px-3 py-2 font-mono text-xs">
                {secret}
              </code>
              <div className="mt-3 flex flex-wrap items-center gap-2">
                <Button variant="secondary" size="sm" onClick={copySecret}>
                  {copied ? <Check className="h-3.5 w-3.5" /> : <Copy className="h-3.5 w-3.5" />}
                  {copied ? "Copied" : "Copy key"}
                </Button>
                <Button variant="ghost" size="sm" onClick={() => setSecret(null)}>
                  Dismiss
                </Button>
              </div>
            </CardContent>
          </Card>
        )}

        <div className="flex flex-col gap-3 xl:flex-row xl:items-end xl:justify-between">
          <div className="grid grid-cols-2 gap-3 md:grid-cols-5 xl:min-w-[48rem]">
            <Stat label="Keys" value={formatNumber(keys.length)} />
            <Stat label="Active" value={formatNumber(activeCount)} />
            <Stat label="Budgeted" value={formatNumber(budgetedCount)} />
            <Stat label="Tenants" value={formatNumber(tenants.length)} />
            <Stat label="Recent tokens" value={formatNumber(recentTraffic)} />
          </div>
          <CreateKeyDialog
            open={createOpen}
            setOpen={setCreateOpen}
            pending={pending}
            tenants={tenants}
            createError={createError}
            onCreate={createKey}
          />
        </div>

        <TopKeysPanel rows={rows} refreshing={refreshing} onRefresh={refreshData} />

        <Card>
          <CardHeader className="gap-4">
            <div className="flex flex-col gap-3 2xl:flex-row 2xl:items-start 2xl:justify-between">
              <div>
                <CardTitle>API keys</CardTitle>
                <CardDescription>
                  Showing {formatNumber(sorted.length)} of {formatNumber(keys.length)} loaded keys
                </CardDescription>
              </div>
              <div className="grid gap-2 sm:grid-cols-2 lg:grid-cols-[minmax(16rem,1fr)_minmax(11rem,13rem)_minmax(9rem,10rem)_minmax(9rem,10rem)_minmax(7rem,8rem)] 2xl:w-[58rem]">
                <div className="relative sm:col-span-2 lg:col-span-1">
                  <Search className="pointer-events-none absolute left-2.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" />
                  <Input
                    type="search"
                    value={query}
                    onChange={(e) => onFilterChange(setQuery)(e.target.value)}
                    placeholder="Search key, tenant, prefix, description..."
                    aria-label="Search API keys"
                    className="h-9 pl-8 text-xs"
                  />
                </div>
                <Select
                  value={tenantFilter}
                  onChange={(e) => onFilterChange(setTenantFilter)(e.target.value)}
                  aria-label="Filter API keys by tenant"
                  className="h-9 text-xs"
                >
                  <option value="all">All tenants</option>
                  {tenants.map((t) => (
                    <option key={t.id} value={t.id}>
                      {t.name}
                    </option>
                  ))}
                </Select>
                <Select
                  value={statusFilter}
                  onChange={(e) => onFilterChange(setStatusFilter)(e.target.value as StatusFilter)}
                  aria-label="Filter API keys by status"
                  className="h-9 text-xs"
                >
                  <option value="all">All status</option>
                  <option value="active">Active</option>
                  <option value="disabled">Disabled</option>
                </Select>
                <Select
                  value={budgetFilter}
                  onChange={(e) => onFilterChange(setBudgetFilter)(e.target.value as BudgetFilter)}
                  aria-label="Filter API keys by budget"
                  className="h-9 text-xs"
                >
                  <option value="all">All budgets</option>
                  <option value="budgeted">Budgeted</option>
                  <option value="unlimited">Unlimited</option>
                </Select>
                <Select
                  value={String(pageSize)}
                  onChange={(e) => {
                    setPageSize(Number(e.target.value));
                    setPage(0);
                  }}
                  aria-label="Rows per page"
                  className="h-9 text-xs"
                >
                  {PAGE_SIZE_OPTIONS.map((size) => (
                    <option key={size} value={size}>
                      {size} rows
                    </option>
                  ))}
                </Select>
              </div>
            </div>

            <div className="flex flex-wrap items-center gap-2 rounded-md border border-border bg-background/35 px-3 py-2">
              <SlidersHorizontal className="h-3.5 w-3.5 text-muted-foreground" />
              <span className="mr-auto text-xs tabular-nums text-muted-foreground">
                {selectedCount > 0
                  ? `${formatNumber(selectedCount)} selected`
                  : `Page ${safePage + 1} of ${pageCount} / ${formatNumber(pageRows.length)} rows`}
              </span>
              <Button type="button" variant="outline" size="sm" disabled={pageRows.length === 0 || pending} onClick={togglePageSelection}>
                {pageSelected ? "Clear page" : "Select page"}
              </Button>
              <Button type="button" variant="outline" size="sm" disabled={selectedCount === 0 || pending} onClick={() => setSelectedIds(new Set())}>
                Clear
              </Button>
              <Button type="button" variant="destructive" size="sm" disabled={selectedCount === 0 || pending} onClick={deleteSelectedKeys}>
                <Trash2 className="h-3.5 w-3.5" />
                Delete selected
              </Button>
              <Button type="button" variant="destructive" size="sm" disabled={!hasActiveFilter || sorted.length === 0 || pending} onClick={deleteFilteredKeys}>
                <Trash2 className="h-3.5 w-3.5" />
                Delete filtered
              </Button>
            </div>
          </CardHeader>

          <CardContent className="p-0">
            <div className="overflow-x-auto">
              <table className="w-full min-w-[1180px] table-fixed text-sm">
                <thead>
                  <tr className="border-y border-border bg-card/40 text-left text-xs text-muted-foreground">
                    <th className="w-10 px-4 py-2">
                      <input
                        type="checkbox"
                        aria-label={pageSelected ? "Clear page selection" : "Select page"}
                        checked={pageSelected}
                        disabled={pageRows.length === 0 || pending}
                        onChange={togglePageSelection}
                        className="rounded border-border"
                      />
                    </th>
                    <th className="w-10 px-2 py-2" />
                    <SortHeader label="Tenant" sortKey="tenant" sort={sort} onSort={changeSort} className="w-[15%]" />
                    <SortHeader label="Key" sortKey="name" sort={sort} onSort={changeSort} className="w-[19%]" />
                    <SortHeader label="Status" sortKey="status" sort={sort} onSort={changeSort} className="w-[8%]" />
                    <SortHeader label="Budget" sortKey="budget" sort={sort} onSort={changeSort} className="w-[11%]" />
                    <SortHeader label="Last used" sortKey="lastUsed" sort={sort} onSort={changeSort} className="w-[10%]" />
                    <SortHeader label="Requests" sortKey="requests" sort={sort} onSort={changeSort} align="right" className="w-[9%]" />
                    <SortHeader label="Tokens" sortKey="tokens" sort={sort} onSort={changeSort} align="right" className="w-[10%]" />
                    <SortHeader label="Cost" sortKey="cost" sort={sort} onSort={changeSort} align="right" className="w-[8%]" />
                    <th className="w-[10%] px-3 py-2 font-medium">Actions</th>
                  </tr>
                </thead>
                <tbody>
                  {pageRows.map((row) => {
                    const { key } = row;
                    const expanded = expandedId === key.id;
                    return (
                      <Fragment key={key.id}>
                        <tr className={cn("border-b border-border/70", expanded && "bg-foreground/[0.03]")}>
                          <td className="px-4 py-2">
                            <input
                              type="checkbox"
                              aria-label={`Select API key ${key.name}`}
                              checked={selectedIds.has(key.id)}
                              disabled={pending}
                              onChange={(e) => toggleKeySelection(key.id, e.target.checked)}
                              className="rounded border-border"
                            />
                          </td>
                          <td className="px-2 py-2">
                            <Button
                              type="button"
                              variant="ghost"
                              size="icon"
                              className="h-7 w-7"
                              aria-label={expanded ? `Collapse ${key.name}` : `Expand ${key.name}`}
                              onClick={() => setExpandedId(expanded ? null : key.id)}
                            >
                              {expanded ? <ChevronDown className="h-3.5 w-3.5" /> : <ChevronRight className="h-3.5 w-3.5" />}
                            </Button>
                          </td>
                          <td className="px-3 py-2">
                            <p className="truncate font-medium">{row.tenantName}</p>
                            <p className="truncate text-[11px] text-muted-foreground">{key.tenant_id.slice(0, 8)}</p>
                          </td>
                          <td className="px-3 py-2">
                            <p className="truncate font-medium">{key.name}</p>
                            <p className="truncate font-mono text-[11px] text-muted-foreground">{key.key_prefix}...</p>
                          </td>
                          <td className="px-3 py-2">
                            <Badge className={key.disabled ? "opacity-50" : "border-emerald-500/40 text-emerald-500"}>
                              {key.disabled ? "disabled" : "active"}
                            </Badge>
                          </td>
                          <td className="px-3 py-2">
                            <BudgetChip apiKey={key} usage={row.usage} />
                          </td>
                          <td className="px-3 py-2 text-xs text-muted-foreground" title={row.usage?.last_model ? `Last model: ${row.usage.last_model}` : undefined}>
                            {formatLastUsed(row.usage?.last_used_ms)}
                          </td>
                          <td className="px-3 py-2 text-right tabular-nums">{formatNumber(Number(row.usage?.requests ?? 0))}</td>
                          <td className="px-3 py-2 text-right tabular-nums">{formatNumber(Number(row.usage?.total_tokens ?? 0))}</td>
                          <td className="px-3 py-2 text-right tabular-nums">{formatCurrency(Number(row.usage?.cost_usd ?? 0))}</td>
                          <td className="px-3 py-2">
                            <div className="flex items-center gap-1">
                              <Button variant="ghost" size="sm" disabled={pending} onClick={() => start(() => toggleKeyAction(key.id, !key.disabled))}>
                                {key.disabled ? "Enable" : "Disable"}
                              </Button>
                              <Button variant="ghost" size="icon" className="h-8 w-8 text-destructive" disabled={pending} onClick={() => removeKey(key)}>
                                <Trash2 className="h-3.5 w-3.5" />
                              </Button>
                            </div>
                          </td>
                        </tr>
                        {expanded && (
                          <tr key={`${key.id}-details`} className="border-b border-border bg-foreground/[0.02]">
                            <td colSpan={11} className="px-4 py-4">
                              <KeyDetailPanel
                                row={row}
                                pending={pending}
                                message={rowMessages[key.id]}
                                onSave={saveKey}
                              />
                            </td>
                          </tr>
                        )}
                      </Fragment>
                    );
                  })}
                  {sorted.length === 0 && (
                    <tr>
                      <td colSpan={11} className="px-6 py-12 text-center text-muted-foreground">
                        {keys.length === 0 ? "No keys yet." : "No keys match your filters."}
                      </td>
                    </tr>
                  )}
                </tbody>
              </table>
            </div>

            {pageCount > 1 && (
              <div className="flex flex-col gap-2 border-t border-border px-6 py-3 text-xs text-muted-foreground sm:flex-row sm:items-center sm:justify-between">
                <span>
                  Showing {safePage * pageSize + 1}-{Math.min((safePage + 1) * pageSize, sorted.length)} of{" "}
                  {formatNumber(sorted.length)}
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
      </div>
    </TooltipProvider>
  );
}

function CreateKeyDialog({
  open,
  setOpen,
  pending,
  tenants,
  createError,
  onCreate,
}: {
  open: boolean;
  setOpen: (open: boolean) => void;
  pending: boolean;
  tenants: Tenant[];
  createError: string | null;
  onCreate: (formData: FormData) => void;
}) {
  return (
    <Dialog open={open} onOpenChange={setOpen}>
      <DialogTrigger asChild>
        <Button disabled={tenants.length === 0}>
          <Plus className="h-4 w-4" />
          New key
        </Button>
      </DialogTrigger>
      <DialogContent className="grid h-[min(680px,85vh)] max-h-[85vh] max-w-3xl grid-rows-[auto_minmax(0,1fr)] overflow-hidden">
        <DialogHeader>
          <DialogTitle>Create API key</DialogTitle>
          <DialogDescription>Issue a tenant-scoped key with optional per-key caps.</DialogDescription>
        </DialogHeader>
        <form action={onCreate} className="grid min-h-0 grid-rows-[minmax(0,1fr)_auto] gap-4">
          <div className="min-h-0 space-y-5 overflow-y-auto pr-1">
            <div className="grid gap-4 md:grid-cols-2">
              <Field label="Tenant" htmlFor="new-key-tenant">
                <Select id="new-key-tenant" name="tenant_id" required disabled={tenants.length === 0}>
                  {tenants.map((t) => (
                    <option key={t.id} value={t.id}>
                      {t.name}
                    </option>
                  ))}
                </Select>
              </Field>
              <Field label="Key name" htmlFor="new-key-name">
                <Input id="new-key-name" name="name" placeholder="prod-chat" required />
              </Field>
            </div>
            <Field label="Description" htmlFor="new-key-description">
              <textarea
                id="new-key-description"
                name="description"
                rows={4}
                placeholder="Owner, workload, environment, or rotation notes"
                className="min-h-24 w-full rounded-md border border-input bg-background px-3 py-2 text-sm shadow-sm outline-none transition-colors placeholder:text-muted-foreground focus-visible:ring-1 focus-visible:ring-ring"
              />
            </Field>
            <BudgetFields />
            {createError && <p className="rounded-md border border-destructive/40 px-3 py-2 text-sm text-destructive">{createError}</p>}
            {tenants.length === 0 && (
              <p className="rounded-md border border-border px-3 py-2 text-sm text-muted-foreground">
                Create a tenant before issuing API keys.
              </p>
            )}
          </div>
          <DialogFooter className="border-t border-border pt-4">
            <Button type="button" variant="ghost" onClick={() => setOpen(false)} disabled={pending}>
              Cancel
            </Button>
            <Button type="submit" disabled={pending || tenants.length === 0} className="border border-foreground/25">
              <KeyRound className="h-4 w-4" />
              {pending ? "Creating..." : "Create key"}
            </Button>
          </DialogFooter>
        </form>
      </DialogContent>
    </Dialog>
  );
}

function TopKeysPanel({
  rows,
  refreshing,
  onRefresh,
}: {
  rows: KeyRow[];
  refreshing: boolean;
  onRefresh: () => void;
}) {
  const topRows = useMemo(
    () =>
      rows
        .filter((row) => Number(row.usage?.requests ?? 0) > 0)
        .sort((a, b) => Number(b.usage?.total_tokens ?? 0) - Number(a.usage?.total_tokens ?? 0))
        .slice(0, 10),
    [rows],
  );
  const maxTokens = Math.max(1, ...topRows.map((row) => Number(row.usage?.total_tokens ?? 0)));
  const totalTokens = topRows.reduce((sum, row) => sum + Number(row.usage?.total_tokens ?? 0), 0);
  const totalCost = topRows.reduce((sum, row) => sum + Number(row.usage?.cost_usd ?? 0), 0);
  const leader = topRows[0];

  return (
    <Card>
      <CardHeader className="gap-3">
        <div className="flex flex-col gap-3 lg:flex-row lg:items-start lg:justify-between">
          <div>
            <CardTitle>Top 10 keys</CardTitle>
            <CardDescription>Highest token traffic in the current usage window.</CardDescription>
          </div>
          <Button type="button" variant="outline" size="sm" disabled={refreshing} onClick={onRefresh}>
            <RefreshCw className={cn("h-3.5 w-3.5", refreshing && "animate-spin")} />
            {refreshing ? "Refreshing" : "Refresh"}
          </Button>
        </div>
        {leader && (
          <div className="grid gap-3 md:grid-cols-3">
            <TopKeyStat label="Leader" value={leader.key.name} detail={leader.tenantName} />
            <TopKeyStat label="Top 10 tokens" value={formatNumber(totalTokens)} detail={`${topRows.length} active keys`} />
            <TopKeyStat label="Top 10 cost" value={formatCurrency(totalCost)} detail="recent window" />
          </div>
        )}
      </CardHeader>
      <CardContent>
        {topRows.length === 0 ? (
          <div className="rounded-md border border-dashed border-border px-6 py-8 text-sm text-muted-foreground">
            No key traffic in the current window.
          </div>
        ) : (
          <div className="space-y-2">
            {topRows.map((row, idx) => (
              <TopKeyRow key={row.key.id} row={row} rank={idx + 1} maxTokens={maxTokens} />
            ))}
          </div>
        )}
      </CardContent>
    </Card>
  );
}

function TopKeyRow({ row, rank, maxTokens }: { row: KeyRow; rank: number; maxTokens: number }) {
  const tokens = Number(row.usage?.total_tokens ?? 0);
  const requests = Number(row.usage?.requests ?? 0);
  const cost = Number(row.usage?.cost_usd ?? 0);
  const width = Math.max(3, (tokens / maxTokens) * 100);
  return (
    <div className="grid gap-3 rounded-md border border-border bg-background/40 px-3 py-3 lg:grid-cols-[2.75rem_minmax(14rem,1fr)_minmax(18rem,1.15fr)_minmax(18rem,0.85fr)] lg:items-center">
      <div className="flex h-9 w-9 items-center justify-center rounded-md border border-border bg-card text-sm font-semibold tabular-nums">
        {rank}
      </div>
      <div className="min-w-0">
        <div className="flex flex-wrap items-center gap-2">
          <p className="truncate font-semibold">{row.key.name}</p>
          <BudgetChip apiKey={row.key} usage={row.usage} />
        </div>
        <p className="mt-1 truncate text-xs text-muted-foreground">{row.tenantName}</p>
      </div>
      <div className="min-w-0">
        <div className="mb-1 flex items-center justify-between gap-3 text-xs">
          <span className="text-muted-foreground">Token share</span>
          <span className="font-medium tabular-nums">{formatNumber(tokens)}</span>
        </div>
        <div className="h-2 overflow-hidden rounded-full bg-muted">
          <div className="h-full rounded-full bg-foreground" style={{ width: `${width}%` }} />
        </div>
      </div>
      <div className="grid grid-cols-3 gap-2 text-xs">
        <MiniMetric label="Requests" value={formatNumber(requests)} />
        <MiniMetric label="Cost" value={formatCurrency(cost)} />
        <MiniMetric label="Last" value={formatLastUsed(row.usage?.last_used_ms)} />
      </div>
    </div>
  );
}

function TopKeyStat({ label, value, detail }: { label: string; value: string; detail: string }) {
  return (
    <div className="rounded-md border border-border bg-background/40 px-3 py-2">
      <p className="text-[10px] uppercase tracking-wider text-muted-foreground">{label}</p>
      <p className="mt-1 truncate text-sm font-semibold">{value}</p>
      <p className="mt-0.5 truncate text-xs text-muted-foreground">{detail}</p>
    </div>
  );
}

function MiniMetric({ label, value }: { label: string; value: string }) {
  return (
    <div className="min-w-0 rounded-md border border-border bg-card/40 px-2 py-1.5">
      <p className="text-[10px] uppercase tracking-wider text-muted-foreground">{label}</p>
      <p className="truncate text-xs font-medium tabular-nums">{value}</p>
    </div>
  );
}

function KeyDetailPanel({
  row,
  pending,
  message,
  onSave,
}: {
  row: KeyRow;
  pending: boolean;
  message?: RowMessage;
  onSave: (formData: FormData) => void;
}) {
  const { key, usage } = row;
  const [tracingPending, startTracing] = useTransition();
  return (
    <div className="grid gap-4 xl:grid-cols-[minmax(26rem,1fr)_minmax(18rem,0.55fr)]">
      <form action={onSave} className="rounded-md border border-border bg-background/60 p-4">
        <input type="hidden" name="id" value={key.id} />
        <input type="hidden" name="budget_started_at" value={key.budget_started_at ?? ""} />
        <div className="grid gap-3 md:grid-cols-2">
          <Field label="Name" htmlFor={`key-name-${key.id}`}>
            <Input id={`key-name-${key.id}`} name="name" defaultValue={key.name} required />
          </Field>
          <Field label="Budget period" htmlFor={`key-budget-period-${key.id}`}>
            <Select id={`key-budget-period-${key.id}`} name="budget_period" defaultValue={key.budget_period ?? "lifetime"}>
              <option value="lifetime">Lifetime</option>
              <option value="monthly">Monthly</option>
              <option value="term">Term</option>
            </Select>
          </Field>
          <Field label="Description" htmlFor={`key-description-${key.id}`} className="md:col-span-2">
            <textarea
              id={`key-description-${key.id}`}
              name="description"
              rows={3}
              defaultValue={key.description ?? ""}
              placeholder="Owner, workload, environment, or rotation notes"
              className="min-h-20 w-full rounded-md border border-input bg-background px-3 py-2 text-sm shadow-sm outline-none transition-colors placeholder:text-muted-foreground focus-visible:ring-1 focus-visible:ring-ring"
            />
          </Field>
          <Field
            label="Token cap"
            info="Optional cumulative token cap for this API key. Leave blank for unlimited."
            htmlFor={`key-budget-tokens-${key.id}`}
          >
            <Input
              id={`key-budget-tokens-${key.id}`}
              name="budget_tokens"
              type="number"
              min={0}
              step={1}
              defaultValue={key.budget_tokens ?? ""}
              placeholder="Unlimited"
            />
          </Field>
          <Field
            label="Cost cap"
            info="Optional cumulative USD cap for this API key. Leave blank for unlimited."
            htmlFor={`key-budget-cost-${key.id}`}
          >
            <Input
              id={`key-budget-cost-${key.id}`}
              name="budget_cost_usd"
              type="number"
              min={0}
              step="0.01"
              defaultValue={key.budget_cost_usd ?? ""}
              placeholder="Unlimited"
            />
          </Field>
        </div>
        <div className="mt-3 flex flex-wrap items-center justify-between gap-2">
          <StatusMessage message={message} />
          <Button type="submit" size="sm" disabled={pending} className="border border-foreground/25">
            <Save className="h-3.5 w-3.5" />
            Save key
          </Button>
        </div>
      </form>

      <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-1">
        <BudgetMeter apiKey={key} usage={usage} />
        <div className="rounded-md border border-border bg-background/60 p-4">
          <p className="mb-3 text-sm font-semibold">Usage snapshot</p>
          <div className="grid grid-cols-2 gap-2 text-xs">
            <UsageStat label="Requests" value={formatNumber(Number(usage?.requests ?? 0))} />
            <UsageStat label="Tokens" value={formatNumber(Number(usage?.total_tokens ?? 0))} />
            <UsageStat label="Cost" value={formatCurrency(Number(usage?.cost_usd ?? 0))} />
            <UsageStat label="Last used" value={formatLastUsed(usage?.last_used_ms)} />
          </div>
        </div>
        <div className="rounded-md border border-border bg-background/60 p-4">
          <p className="mb-1 text-sm font-semibold">Request tracing</p>
          <p className="mb-3 text-xs text-muted-foreground">
            Record per-hop span data for requests made with this key.
          </p>
          <Button
            type="button"
            variant={key.tracing_enabled ? "default" : "outline"}
            size="sm"
            disabled={pending || tracingPending}
            className={key.tracing_enabled ? "border border-emerald-500/40 bg-emerald-950/40 text-emerald-400 hover:bg-emerald-950/60" : ""}
            onClick={() => startTracing(() => toggleKeyTracingAction(key.id, !key.tracing_enabled))}
          >
            {key.tracing_enabled ? "⬡ Tracing on" : "Tracing off"}
          </Button>
        </div>
      </div>
    </div>
  );
}

function SortHeader({
  label,
  sortKey,
  sort,
  onSort,
  align = "left",
  className,
}: {
  label: string;
  sortKey: SortKey;
  sort: SortState;
  onSort: (key: SortKey) => void;
  align?: "left" | "right";
  className?: string;
}) {
  const active = sort.key === sortKey;
  const Icon = active ? (sort.direction === "asc" ? ArrowUp : ArrowDown) : ArrowUpDown;
  return (
    <th className={cn("px-3 py-2 font-medium", align === "right" && "text-right", className)}>
      <button
        type="button"
        onClick={() => onSort(sortKey)}
        className={cn("inline-flex items-center gap-1 hover:text-foreground", align === "right" && "justify-end")}
      >
        {label}
        <Icon className="h-3 w-3" />
      </button>
    </th>
  );
}

function BudgetFields() {
  return (
    <div className="rounded-md border border-border bg-card/40 p-4">
      <div className="mb-3 flex items-center gap-2">
        <h3 className="text-sm font-semibold">Per-key budget</h3>
        <InfoTooltip text="Budgets are optional. Leave token and cost caps blank to keep this key unlimited." />
      </div>
      <div className="grid gap-4 md:grid-cols-3">
        <Field label="Token cap" htmlFor="new-key-budget-tokens">
          <Input id="new-key-budget-tokens" name="budget_tokens" type="number" min={0} step={1} placeholder="Unlimited" />
        </Field>
        <Field label="Cost cap" htmlFor="new-key-budget-cost">
          <Input id="new-key-budget-cost" name="budget_cost_usd" type="number" min={0} step="0.01" placeholder="Unlimited" />
        </Field>
        <Field label="Period" htmlFor="new-key-budget-period">
          <Select id="new-key-budget-period" name="budget_period" defaultValue="lifetime">
            <option value="lifetime">Lifetime</option>
            <option value="monthly">Monthly</option>
            <option value="term">Term</option>
          </Select>
        </Field>
      </div>
    </div>
  );
}

function BudgetChip({ apiKey, usage }: { apiKey: ApiKey; usage?: KeyUsageSummary }) {
  if (!hasKeyBudget(apiKey)) return <Badge>Unlimited</Badge>;
  const pct = budgetPercent(apiKey, usage);
  return (
    <div className="min-w-0">
      <div className="flex items-center gap-2">
        <Badge className={pct >= 90 ? "border-destructive/50 text-destructive" : ""}>{Math.round(pct)}%</Badge>
        <span className="truncate text-[11px] text-muted-foreground">{budgetPeriodLabel(apiKey.budget_period)}</span>
      </div>
      <div className="mt-1 h-1 overflow-hidden rounded-full bg-muted">
        <div
          className={cn("h-full rounded-full", pct >= 90 ? "bg-destructive" : pct >= 70 ? "bg-amber-500" : "bg-foreground")}
          style={{ width: `${Math.min(100, Math.max(5, pct))}%` }}
        />
      </div>
    </div>
  );
}

function BudgetMeter({ apiKey, usage }: { apiKey: ApiKey; usage?: KeyUsageSummary }) {
  if (!hasKeyBudget(apiKey)) {
    return (
      <div className="rounded-md border border-border bg-background/60 p-4">
        <div className="flex items-center justify-between gap-2">
          <span className="text-sm font-semibold">Budget</span>
          <Badge>Unlimited</Badge>
        </div>
        <p className="mt-2 text-xs leading-5 text-muted-foreground">No per-key token or cost cap.</p>
      </div>
    );
  }

  const pct = budgetPercent(apiKey, usage);
  return (
    <div className="rounded-md border border-border bg-background/60 p-4">
      <div className="flex items-start justify-between gap-3">
        <div>
          <p className="text-sm font-semibold">Budget</p>
          <p className="text-xs text-muted-foreground">{budgetPeriodLabel(apiKey.budget_period)}</p>
        </div>
        <Badge className={pct >= 90 ? "border-destructive/50 text-destructive" : ""}>{Math.round(pct)}%</Badge>
      </div>
      <div className="mt-3 h-2 overflow-hidden rounded-full bg-muted">
        <div
          className={cn("h-full rounded-full", pct >= 90 ? "bg-destructive" : pct >= 70 ? "bg-amber-500" : "bg-foreground")}
          style={{ width: `${Math.min(100, Math.max(4, pct))}%` }}
        />
      </div>
      <div className="mt-3 space-y-1 text-xs text-muted-foreground">
        {apiKey.budget_tokens != null && (
          <p>
            {formatNumber(Number(usage?.total_tokens ?? 0))} / {formatNumber(apiKey.budget_tokens)} tokens in recent traffic
          </p>
        )}
        {apiKey.budget_cost_usd != null && (
          <p>
            {formatCurrency(Number(usage?.cost_usd ?? 0))} / {formatCurrency(apiKey.budget_cost_usd)} in recent traffic
          </p>
        )}
      </div>
    </div>
  );
}

function StatusMessage({ message }: { message?: RowMessage }) {
  if (message?.error) return <p className="text-xs text-destructive">{message.error}</p>;
  if (message?.saved) return <p className="text-xs text-emerald-500">Saved.</p>;
  return <p className="text-xs text-muted-foreground">Blank caps mean unlimited.</p>;
}

function Field({
  label,
  info,
  htmlFor,
  children,
  className,
}: {
  label: ReactNode;
  info?: string;
  htmlFor: string;
  children: ReactNode;
  className?: string;
}) {
  return (
    <div className={cn("space-y-1.5", className)}>
      <div className="flex items-center gap-1">
        <Label htmlFor={htmlFor}>{label}</Label>
        {info && <InfoTooltip text={info} />}
      </div>
      {children}
    </div>
  );
}

function InfoTooltip({ text }: { text: string }) {
  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <button type="button" className="inline-flex rounded-full text-muted-foreground hover:text-foreground">
          <Info className="h-3.5 w-3.5" />
          <span className="sr-only">More information</span>
        </button>
      </TooltipTrigger>
      <TooltipContent side="top" align="start" className="max-w-xs leading-relaxed">
        {text}
      </TooltipContent>
    </Tooltip>
  );
}

function UsageStat({ label, value }: { label: string; value: string }) {
  return (
    <div className="rounded-md border border-border bg-card/40 px-3 py-2">
      <p className="text-[10px] uppercase tracking-wider text-muted-foreground">{label}</p>
      <p className="mt-0.5 truncate text-xs font-medium tabular-nums">{value}</p>
    </div>
  );
}

function Stat({ label, value }: { label: string; value: string }) {
  return (
    <div className="rounded-md border border-border bg-card/50 px-4 py-3">
      <p className="text-[10px] uppercase tracking-wider text-muted-foreground">{label}</p>
      <p className="mt-0.5 text-lg font-semibold tabular-nums">{value}</p>
    </div>
  );
}

function hasKeyBudget(key: ApiKey) {
  return key.budget_tokens != null || key.budget_cost_usd != null;
}

function budgetPercent(key: ApiKey, usage?: KeyUsageSummary) {
  const tokenPct =
    key.budget_tokens && key.budget_tokens > 0 ? (Number(usage?.total_tokens ?? 0) / key.budget_tokens) * 100 : 0;
  const costPct =
    key.budget_cost_usd && key.budget_cost_usd > 0 ? (Number(usage?.cost_usd ?? 0) / key.budget_cost_usd) * 100 : 0;
  return Math.max(tokenPct, costPct);
}

function budgetPeriodLabel(period?: string | null) {
  switch (period) {
    case "monthly":
      return "Monthly";
    case "term":
      return "Term";
    default:
      return "Lifetime";
  }
}

function sortValue(row: KeyRow, key: SortKey): string | number {
  switch (key) {
    case "tenant":
      return row.tenantName.toLowerCase();
    case "name":
      return row.key.name.toLowerCase();
    case "status":
      return row.key.disabled ? 1 : 0;
    case "budget":
      return hasKeyBudget(row.key) ? budgetPercent(row.key, row.usage) : -1;
    case "lastUsed":
      return Number(row.usage?.last_used_ms ?? 0);
    case "requests":
      return Number(row.usage?.requests ?? 0);
    case "tokens":
      return Number(row.usage?.total_tokens ?? 0);
    case "cost":
      return Number(row.usage?.cost_usd ?? 0);
    case "created":
      return Date.parse(row.key.created_at) || 0;
  }
}

function compareSortValues(a: string | number, b: string | number) {
  if (typeof a === "string" && typeof b === "string") return a.localeCompare(b);
  return Number(a) - Number(b);
}

function defaultSortDirection(key: SortKey): SortDirection {
  return key === "tenant" || key === "name" || key === "status" ? "asc" : "desc";
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
