"use client";

import { useMemo, useState, useTransition } from "react";
import { Check, Copy, Trash2 } from "lucide-react";
import {
  createKeyAction,
  deleteFilteredKeysAction,
  deleteKeyAction,
  deleteKeysAction,
  toggleKeyAction,
} from "@/app/actions";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Select } from "@/components/ui/select";
import type { ApiKey, Tenant, UsageKeyAgg } from "@/lib/obleth";
import { KeyUsageChart } from "@/components/fairshare-dashboard";
import { formatNumber } from "@/lib/utils";

const PAGE_SIZE = 50;

type StatusFilter = "all" | "active" | "disabled";

export function KeyManager({
  tenants,
  keys,
  usageByKey,
}: {
  tenants: Tenant[];
  keys: ApiKey[];
  usageByKey: UsageKeyAgg[];
}) {
  const [secret, setSecret] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
  const [pending, start] = useTransition();

  const [query, setQuery] = useState("");
  const [tenantFilter, setTenantFilter] = useState("all");
  const [statusFilter, setStatusFilter] = useState<StatusFilter>("all");
  const [page, setPage] = useState(0);
  const [selectedIds, setSelectedIds] = useState<Set<string>>(() => new Set());

  const tenantNameMap = useMemo(() => Object.fromEntries(tenants.map((t) => [t.id, t.name])), [tenants]);
  const tenantName = (id: string) => tenantNameMap[id] ?? id.slice(0, 8);
  const usageMap = useMemo(() => new Map(usageByKey.map((u) => [u.key_id, u])), [usageByKey]);

  const filtered = useMemo(() => {
    const q = query.trim().toLowerCase();
    return keys.filter((k) => {
      if (tenantFilter !== "all" && k.tenant_id !== tenantFilter) return false;
      if (statusFilter === "active" && k.disabled) return false;
      if (statusFilter === "disabled" && !k.disabled) return false;
      if (!q) return true;
      const name = tenantNameMap[k.tenant_id] ?? k.tenant_id.slice(0, 8);
      return (
        k.key_prefix.toLowerCase().includes(q) ||
        k.name.toLowerCase().includes(q) ||
        name.toLowerCase().includes(q)
      );
    });
  }, [keys, query, tenantFilter, statusFilter, tenantNameMap]);

  const pageCount = Math.max(1, Math.ceil(filtered.length / PAGE_SIZE));
  const safePage = Math.min(page, pageCount - 1);
  const pageRows = filtered.slice(safePage * PAGE_SIZE, safePage * PAGE_SIZE + PAGE_SIZE);
  const selectedCount = selectedIds.size;
  const pageSelected = pageRows.length > 0 && pageRows.every((key) => selectedIds.has(key.id));
  const hasActiveFilter = query.trim() !== "" || tenantFilter !== "all" || statusFilter !== "all";

  const onFilterChange = <T,>(setter: (v: T) => void) => (v: T) => {
    setter(v);
    setPage(0);
  };

  const activeCount = keys.filter((k) => !k.disabled).length;

  async function copySecret() {
    if (!secret) return;
    await navigator.clipboard.writeText(secret);
    setCopied(true);
    window.setTimeout(() => setCopied(false), 1500);
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
        for (const key of pageRows) next.delete(key.id);
      } else {
        for (const key of pageRows) next.add(key.id);
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
    if (!hasActiveFilter || filtered.length === 0) return;
    if (!window.confirm(`Delete ${filtered.length} filtered API keys? This cannot be undone.`)) return;
    start(async () => {
      const result = await deleteFilteredKeysAction({
        query,
        tenantId: tenantFilter,
        status: statusFilter,
      });
      setSelectedIds(new Set());
      if (result.failed > 0) {
        window.alert(`Deleted ${result.deleted} of ${result.matched} matched keys; ${result.failed} failed.`);
      }
    });
  }

  return (
    <div className="space-y-6">
      {secret && (
        <Card className="border-foreground/20">
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

      <div className="grid grid-cols-2 gap-3 sm:grid-cols-4">
        <Stat label="Total keys" value={formatNumber(keys.length)} />
        <Stat label="Active" value={formatNumber(activeCount)} />
        <Stat label="Disabled" value={formatNumber(keys.length - activeCount)} />
        <Stat label="Tenants" value={formatNumber(tenants.length)} />
      </div>

      <Card>
        <CardHeader>
          <CardTitle>Create API key</CardTitle>
          <CardDescription>Keys authenticate against the OpenAI-compatible data plane</CardDescription>
        </CardHeader>
        <CardContent>
          <form
            action={(fd) =>
              start(async () => {
                const s = await createKeyAction(fd);
                setSecret(s);
                setCopied(false);
              })
            }
            className="flex flex-wrap items-end gap-3"
          >
            <div className="space-y-1.5">
              <Label htmlFor="key-tenant">Tenant</Label>
              <Select id="key-tenant" name="tenant_id" required disabled={tenants.length === 0} className="w-48">
                {tenants.map((t) => (
                  <option key={t.id} value={t.id}>
                    {t.name}
                  </option>
                ))}
              </Select>
            </div>
            <div className="space-y-1.5">
              <Label htmlFor="key-name">Key name</Label>
              <Input id="key-name" name="name" placeholder="prod" className="w-40" />
            </div>
            <Button type="submit" disabled={pending || tenants.length === 0}>
              {pending ? "..." : "Create key"}
            </Button>
          </form>
          {tenants.length === 0 && (
            <p className="mt-3 text-xs text-muted-foreground">Create a tenant before issuing API keys.</p>
          )}
        </CardContent>
      </Card>

      <KeyUsageChart keys={keys} usageByKey={usageByKey} tenantNames={tenantNameMap} />

      <Card>
        <CardHeader className="gap-3">
          <div className="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
            <div>
              <CardTitle>API keys</CardTitle>
              <CardDescription>
                {formatNumber(filtered.length)} of {formatNumber(keys.length)} keys
                {pageCount > 1 ? ` / page ${safePage + 1} of ${pageCount}` : ""}
              </CardDescription>
            </div>
            <div className="flex flex-wrap items-center gap-2">
              <Input
                type="search"
                value={query}
                onChange={(e) => onFilterChange(setQuery)(e.target.value)}
                placeholder="Search prefix, name, tenant..."
                aria-label="Search API keys"
                className="h-8 w-52 text-xs"
              />
              <Select
                value={tenantFilter}
                onChange={(e) => onFilterChange(setTenantFilter)(e.target.value)}
                aria-label="Filter API keys by tenant"
                className="h-8 max-w-[12rem] text-xs"
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
                className="h-8 w-32 text-xs"
              >
                <option value="all">All status</option>
                <option value="active">Active</option>
                <option value="disabled">Disabled</option>
              </Select>
            </div>
          </div>
          <div className="flex flex-wrap items-center gap-2 rounded-md border border-border bg-background/35 px-3 py-2">
            <span className="mr-auto text-xs tabular-nums text-muted-foreground">
              {selectedCount > 0 ? `${formatNumber(selectedCount)} selected` : `${formatNumber(pageRows.length)} on page`}
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
            <Button type="button" variant="destructive" size="sm" disabled={!hasActiveFilter || filtered.length === 0 || pending} onClick={deleteFilteredKeys}>
              <Trash2 className="h-3.5 w-3.5" />
              Delete filtered
            </Button>
          </div>
        </CardHeader>
        <CardContent className="p-0">
          <div className="overflow-x-auto">
            <table className="w-full text-sm">
              <thead>
                <tr className="border-b border-border text-left text-xs text-muted-foreground">
                  <th className="w-10 px-6 py-3 font-medium">
                    <input
                      type="checkbox"
                      aria-label={pageSelected ? "Clear page selection" : "Select page"}
                      checked={pageSelected}
                      disabled={pageRows.length === 0 || pending}
                      onChange={togglePageSelection}
                      className="rounded border-border"
                    />
                  </th>
                  <th className="px-3 py-3 font-medium">Prefix</th>
                  <th className="px-3 py-3 font-medium">Tenant</th>
                  <th className="px-3 py-3 font-medium">Name</th>
                  <th className="px-3 py-3 text-right font-medium">Requests</th>
                  <th className="px-3 py-3 text-right font-medium">Tokens</th>
                  <th className="px-3 py-3 font-medium">Status</th>
                  <th className="px-3 py-3 font-medium" />
                </tr>
              </thead>
              <tbody>
                {pageRows.map((k) => {
                  const u = usageMap.get(k.id);
                  return (
                    <tr key={k.id} className="border-b border-border/60">
                      <td className="px-6 py-3">
                        <input
                          type="checkbox"
                          aria-label={`Select API key ${k.key_prefix}`}
                          checked={selectedIds.has(k.id)}
                          disabled={pending}
                          onChange={(e) => toggleKeySelection(k.id, e.target.checked)}
                          className="rounded border-border"
                        />
                      </td>
                      <td className="px-3 py-3 font-mono text-xs">{k.key_prefix}...</td>
                      <td className="px-3 py-3">{tenantName(k.tenant_id)}</td>
                      <td className="px-3 py-3">{k.name}</td>
                      <td className="px-3 py-3 text-right tabular-nums">{formatNumber(Number(u?.requests ?? 0))}</td>
                      <td className="px-3 py-3 text-right tabular-nums">{formatNumber(Number(u?.total_tokens ?? 0))}</td>
                      <td className="px-3 py-3">
                        <Badge className={k.disabled ? "opacity-50" : ""}>{k.disabled ? "disabled" : "active"}</Badge>
                      </td>
                      <td className="px-3 py-3">
                        <div className="flex justify-end gap-1">
                          <Button
                            variant="ghost"
                            size="sm"
                            disabled={pending}
                            onClick={() => start(() => toggleKeyAction(k.id, !k.disabled))}
                          >
                            {k.disabled ? "Enable" : "Disable"}
                          </Button>
                          <Button
                            variant="ghost"
                            size="sm"
                            className="text-destructive"
                            disabled={pending}
                            onClick={() => removeKey(k)}
                          >
                            <Trash2 className="h-3.5 w-3.5" />
                            Delete
                          </Button>
                        </div>
                      </td>
                    </tr>
                  );
                })}
                {filtered.length === 0 && (
                  <tr>
                    <td colSpan={8} className="px-6 py-10 text-center text-muted-foreground">
                      {keys.length === 0 ? "No keys yet." : "No keys match your filters."}
                    </td>
                  </tr>
                )}
              </tbody>
            </table>
          </div>
          {pageCount > 1 && (
            <div className="flex items-center justify-between border-t border-border px-6 py-3 text-xs text-muted-foreground">
              <span>
                Showing {safePage * PAGE_SIZE + 1}-{Math.min((safePage + 1) * PAGE_SIZE, filtered.length)} of{" "}
                {formatNumber(filtered.length)}
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
  );
}

function Stat({ label, value }: { label: string; value: string }) {
  return (
    <div className="rounded-lg border border-border bg-card/50 px-4 py-3">
      <p className="text-[10px] uppercase tracking-wider text-muted-foreground">{label}</p>
      <p className="mt-0.5 text-lg font-semibold tabular-nums">{value}</p>
    </div>
  );
}
