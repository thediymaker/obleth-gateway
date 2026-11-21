"use client";

import { useEffect, useMemo, useState } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { keepPreviousData, useQuery } from "@tanstack/react-query";
import { ChevronDown, ChevronRight, RefreshCw, Search } from "lucide-react";
import { Kpi, Panel, PanelLink, Pill, Segmented } from "@/components/overview/ui";
import { Input } from "@/components/ui/input";
import { ListFooter } from "@/components/ui/list-footer";
import { Select } from "@/components/ui/select";
import {
  countryLabel,
  filterKeyAccess,
  KEY_ACCESS_RANGES,
  keyAccessRange,
  keyAccessRows,
  keyAccessSummary,
  placeLabel,
  REFUSAL_REASON,
  refusalRows,
  type KeyAccessRange,
  type KeyAccessRow,
  type KeyAccessSort,
} from "@/lib/key-access-model";
import { ReportsTabs } from "@/components/reports/reports-tabs";
import { logsHref } from "@/lib/log-links";
import { ago, compact, formatNumber, fullDateTime } from "@/lib/format";
import type { KeyRejectionAgg, KeySource } from "@/lib/obleth";
import { cn, getJson } from "@/lib/utils";

interface KeyAccessData {
  sinceMs: number;
  sources: KeySource[];
  rejections: KeyRejectionAgg[];
}

/** Rows shown before "Show more". */
const PAGE = 100;

const SORTS: { value: KeyAccessSort; label: string }[] = [
  { value: "requests", label: "Busiest" },
  { value: "networks", label: "Most networks" },
  { value: "recent", label: "Latest" },
  { value: "new", label: "New first" },
];

const LOG_WINDOW: Record<KeyAccessRange, "24h" | "7d" | "30d"> = { "24h": "24h", "7d": "7d", "30d": "30d" };

function NewTag({ children = "New" }: { children?: string }) {
  return (
    <Pill className="h-[18px] px-1.5 text-[10.5px]">{children}</Pill>
  );
}

function NetworkName({ name }: { name: string }) {
  return <span className="rounded border border-border bg-muted/40 px-1.5 py-px text-[11px] text-secondary-foreground">{name}</span>;
}

/** The named networks and countries a key was used from. */
function Where({ row }: { row: KeyAccessRow }) {
  if (row.countries.length === 0 && row.names.length === 0) {
    return <span className="text-muted-foreground">{row.unnamedPrivate ? "Private address" : "Unknown"}</span>;
  }
  return (
    <span className="flex flex-wrap gap-1">
      {row.names.slice(0, 3).map((n) => <NetworkName key={n} name={n} />)}
      {row.names.length > 3 && <span className="text-xs text-muted-foreground">+{row.names.length - 3}</span>}
      {row.countries.slice(0, 6).map((c) => (
        <span
          key={c}
          title={`${countryLabel(c)}${row.newCountries.includes(c) ? ": first seen in this window" : ""}`}
          className={cn(
            "rounded border px-1.5 py-px font-mono text-[11px]",
            row.newCountries.includes(c) ? "border-[hsl(38_65%_62%/0.5)] text-[hsl(38_65%_62%)]" : "border-border text-secondary-foreground",
          )}
        >
          {c}
        </span>
      ))}
      {row.countries.length > 6 && <span className="text-xs text-muted-foreground">+{row.countries.length - 6}</span>}
    </span>
  );
}

const GRID = "grid grid-cols-[1.25rem_minmax(0,1.6fr)_4.5rem_4.5rem_minmax(0,1fr)_5.5rem] items-center gap-3";

function KeyRow({ row, range, now, open, onToggle }: { row: KeyAccessRow; range: KeyAccessRange; now: number; open: boolean; onToggle: () => void }) {
  return (
    <li className="border-t border-border">
      <button
        type="button"
        onClick={onToggle}
        aria-expanded={open}
        className={cn(GRID, "w-full px-[18px] py-[9px] text-left text-[13px] hover:bg-muted/30")}
      >
        {open ? <ChevronDown className="h-3.5 w-3.5 text-muted-foreground" /> : <ChevronRight className="h-3.5 w-3.5 text-muted-foreground" />}
        <span className="flex min-w-0 flex-col">
          <span className="flex min-w-0 items-center gap-2">
            <span className="truncate font-medium" title={row.keyName}>{row.keyName || `${row.keyId.slice(0, 8)}…`}</span>
            {(row.newCountries.length > 0 || row.newNetworks > 0) && <NewTag>{row.newCountries.length > 0 ? "New country" : "New network"}</NewTag>}
            {row.refused > 0 && (
              <Pill inverted className="h-[18px] px-1.5 text-[10.5px]">{compact(row.refused)} refused</Pill>
            )}
          </span>
          <span className="truncate text-xs text-muted-foreground">{row.tenantName}</span>
        </span>
        <span className="text-right font-mono text-xs tabular-nums">{compact(row.requests)}</span>
        <span className="text-right font-mono text-xs tabular-nums" title={`${row.distinctIps} distinct addresses`}>
          {row.places.length}
        </span>
        <Where row={row} />
        <span className="text-right text-xs text-muted-foreground" title={row.lastSeenMs ? fullDateTime(row.lastSeenMs) : undefined}>{ago(row.lastSeenMs, now)}</span>
      </button>
      {open && (
        <div className="bg-muted/15 px-[18px] pb-3 pt-1">
          <table className="w-full text-left text-xs">
            <thead className="text-muted-foreground">
              <tr>
                <th className="py-1.5 pr-3 font-medium">Network</th>
                <th className="py-1.5 pr-3 font-medium">Country</th>
                <th className="py-1.5 pr-3 text-right font-medium">Requests</th>
                <th className="py-1.5 pr-3 text-right font-medium">Addresses</th>
                <th className="py-1.5 text-right font-medium">Last seen</th>
              </tr>
            </thead>
            <tbody>
              {row.places.map((p) => (
                <tr key={`${p.network}:${p.country}`} className="border-t border-border/60">
                  <td className="py-1.5 pr-3">
                    <span className="inline-flex items-center gap-2">
                      <span className="font-mono">{p.network}</span>
                      {p.name && <NetworkName name={p.name} />}
                      {p.newNetwork && <NewTag />}
                    </span>
                  </td>
                  <td className="py-1.5 pr-3">
                    <span className={cn("inline-flex items-center gap-2", !p.country && "text-muted-foreground")}>
                      {placeLabel(p.country, p.network)}
                      {p.newCountry && <NewTag />}
                    </span>
                  </td>
                  <td className="py-1.5 pr-3 text-right font-mono tabular-nums">{formatNumber(p.requests)}</td>
                  <td className="py-1.5 pr-3 text-right font-mono tabular-nums">{formatNumber(p.distinctIps)}</td>
                  <td className="py-1.5 text-right text-muted-foreground" title={p.lastSeenMs ? fullDateTime(p.lastSeenMs) : undefined}>{ago(p.lastSeenMs, now)}</td>
                </tr>
              ))}
            </tbody>
          </table>
          <div className="mt-2 flex flex-wrap gap-4 text-[12.5px]">
            <PanelLink href={logsHref({ key: row.keyId, window: LOG_WINDOW[range] })}>Its requests, with each address</PanelLink>
            <PanelLink href={`/keys?key=${row.keyId}`}>Open the key</PanelLink>
          </div>
        </div>
      )}
    </li>
  );
}

/**
 * Where each API key was used from in a window, one row per key with its
 * networks and countries, what was new, and the requests that were refused.
 */
export function KeyAccessView({
  tenants,
  initialRange,
  initialTenant,
  initialSearch,
  initial,
}: {
  tenants: { id: string; name: string }[];
  initialRange: KeyAccessRange;
  initialTenant: string;
  initialSearch: string;
  initial: KeyAccessData | null;
}) {
  const router = useRouter();
  const [range, setRange] = useState<KeyAccessRange>(initialRange);
  const [tenant, setTenant] = useState(initialTenant);
  const [search, setSearch] = useState(initialSearch);
  const [sort, setSort] = useState<KeyAccessSort>("requests");
  const [onlyNew, setOnlyNew] = useState(false);
  const [openKey, setOpenKey] = useState<string | null>(null);
  const [shown, setShown] = useState(PAGE);
  const [refusalsShown, setRefusalsShown] = useState(PAGE);
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const t = setInterval(() => setNow(Date.now()), 30_000);
    return () => clearInterval(t);
  }, []);

  // Keep the URL in step so a filtered view can be shared.
  useEffect(() => {
    const p = new URLSearchParams();
    if (range !== "24h") p.set("range", range);
    if (tenant) p.set("tenant", tenant);
    if (search.trim()) p.set("q", search.trim());
    const qs = p.toString();
    router.replace(`/reports?tab=key-access${qs ? `&${qs}` : ""}`, { scroll: false });
  }, [range, tenant, search, router]);

  const query = useQuery({
    queryKey: ["key-access", range, tenant],
    queryFn: () => {
      const since = Date.now() - keyAccessRange(range).ms;
      return getJson<KeyAccessData>(`/api/live/usage/key-access?since_ms=${since}${tenant ? `&tenant_id=${tenant}` : ""}`);
    },
    initialData: range === initialRange && tenant === initialTenant && initial ? initial : undefined,
    placeholderData: keepPreviousData,
    refetchInterval: range === "24h" ? 60_000 : 300_000,
  });
  const data = query.data;
  const rows = useMemo(() => (data ? keyAccessRows(data.sources, data.rejections, data.sinceMs) : []), [data]);
  const summary = useMemo(() => (data ? keyAccessSummary(rows, data.rejections) : null), [rows, data]);
  const visible = useMemo(() => filterKeyAccess(rows, search, sort, onlyNew), [rows, search, sort, onlyNew]);
  const refusals = useMemo(() => (data ? refusalRows(data.rejections) : []), [data]);
  const words = keyAccessRange(range).words;

  return (
    <div className="mx-auto flex max-w-[1600px] flex-col gap-4">
      <div className="flex flex-wrap items-end justify-between gap-4">
        <div className="min-w-0">
          <h1 className="text-[26px] font-semibold tracking-tight">Reports</h1>
          <p className="mt-1 text-[13px] text-muted-foreground" role="status">
            {summary
              ? `${formatNumber(summary.keys)} keys were used from ${formatNumber(summary.networks)} networks in ${words}.`
              : query.isLoading
                ? "Reading…"
                : "Where keys were used from could not be read."}
          </p>
        </div>
        <div className="flex flex-wrap items-center gap-2.5">
          <Select
            aria-label="Tenant"
            className="h-[34px] w-56"
            value={tenant}
            onValueChange={(v) => { setTenant(v); setShown(PAGE); setRefusalsShown(PAGE); }}
            options={[{ value: "", label: "All tenants" }, ...tenants.map((t) => ({ value: t.id, label: t.name }))]}
          />
          <Segmented label="Time range" value={range} onChange={(v) => { setRange(v); setShown(PAGE); setRefusalsShown(PAGE); }} options={KEY_ACCESS_RANGES.map((r) => ({ value: r.id, label: r.label }))} />
          <button type="button" onClick={() => void query.refetch()} aria-label="Refresh" title="Refresh" className="flex h-[34px] w-[34px] items-center justify-center rounded-lg border border-border text-muted-foreground hover:bg-accent hover:text-foreground">
            <RefreshCw className={cn("h-3.5 w-3.5", query.isFetching && "animate-spin")} />
          </button>
        </div>
      </div>

      <ReportsTabs active="key-access" />

      {summary && (
        <div className={cn("grid grid-cols-2 gap-3 md:grid-cols-4", query.isPlaceholderData && "opacity-60")}>
          <Kpi label={`Keys used · ${range}`} value={formatNumber(summary.keys)} detail={`from ${summary.countries} ${summary.countries === 1 ? "country" : "countries"}`} trend={[]} />
          <Kpi label={`Networks · ${range}`} value={formatNumber(summary.networks)} detail="IPv4 /24 or IPv6 /48" trend={[]} />
          <Kpi
            label="From a new country"
            value={formatNumber(summary.keysInNewCountry)}
            detail={summary.keysInNewCountry ? "keys seen from a country for the first time" : "no key was seen anywhere new"}
            trend={[]}
            emphasis={summary.keysInNewCountry > 0}
          />
          <Kpi
            label={`Refused · ${range}`}
            value={compact(summary.refused)}
            detail={summary.refusedKeys ? `requests with ${summary.refusedKeys} ${summary.refusedKeys === 1 ? "key" : "keys"}` : "no refused requests"}
            trend={[]}
            emphasis={summary.refused > 0}
          />
        </div>
      )}

      <Panel
        title="Where keys are used"
        subtitle="Each key's networks, by name and country. New means first seen in this window. Select a key to see its networks"
        className={cn(query.isPlaceholderData && "opacity-60")}
      >
        <div className="mt-3 flex flex-wrap items-center gap-2.5 px-[18px] pb-3">
          <div className="relative w-full max-w-xs">
            <Search className="pointer-events-none absolute left-2.5 top-1/2 h-3.5 w-3.5 -translate-y-1/2 text-muted-foreground" />
            <Input
              value={search}
              onChange={(e) => { setSearch(e.target.value); setShown(PAGE); }}
              placeholder="Key, tenant, network, name or country"
              aria-label="Search keys"
              className="h-[34px] pl-8"
            />
          </div>
          <Segmented label="Sort" value={sort} onChange={setSort} options={SORTS} />
          <label className="inline-flex cursor-pointer items-center gap-2 text-[12.5px] text-secondary-foreground">
            <input type="checkbox" checked={onlyNew} onChange={(e) => setOnlyNew(e.target.checked)} className="h-3.5 w-3.5 accent-current" />
            Only keys with something new
          </label>
        </div>
        {rows.some((r) => r.unnamedPrivate) && (
          <p className="px-[18px] pb-3 text-[12.5px] text-muted-foreground">
            Private addresses have no country. Name your own networks, such as a campus, VPN or cluster, under{" "}
            <Link href="/settings#networks" className="text-foreground underline-offset-2 hover:underline">Settings › Networks</Link>{" "}
            and they show here by name.
          </p>
        )}
        {visible.length === 0 ? (
          <p className="border-t border-border px-[18px] py-6 text-center text-sm text-muted-foreground">
            {query.isLoading ? <span className="skeleton mx-auto block h-4 w-40 rounded" /> : data ? (rows.length ? "No key matches." : `No key was used in ${words}.`) : "Where keys were used from could not be read."}
          </p>
        ) : (
          <div className="overflow-x-auto pb-2">
            <div className="min-w-[760px]">
              <div className={cn(GRID, "border-t border-border px-[18px] py-2 text-[11.5px] text-muted-foreground")}>
                <span />
                <span>Key</span>
                <span className="text-right">Requests</span>
                <span className="text-right">Networks</span>
                <span>Where</span>
                <span className="text-right">Last used</span>
              </div>
              <ul>
                {visible.slice(0, shown).map((row) => (
                  <KeyRow key={row.keyId} row={row} range={range} now={now} open={openKey === row.keyId} onToggle={() => setOpenKey(openKey === row.keyId ? null : row.keyId)} />
                ))}
              </ul>
              <TableFooter total={visible.length} shown={shown} noun="keys" onMore={() => setShown(shown + PAGE)} />
            </div>
          </div>
        )}
      </Panel>

      <Panel title="Refused" subtitle="Requests with a turned-off key, or from outside a key's allowed networks" className={cn(query.isPlaceholderData && "opacity-60")}>
        {refusals.length === 0 ? (
          <p className="mt-3 border-t border-border px-[18px] py-6 text-center text-sm text-muted-foreground">
            {data ? `No request was refused in ${words}.` : query.isLoading ? <span className="skeleton mx-auto block h-4 w-40 rounded" /> : "Refusals could not be read."}
          </p>
        ) : (
          <div className="mt-3 overflow-x-auto pb-2">
            <table className="w-full min-w-[860px] text-left text-[13px]">
              <thead className="text-[11.5px] text-muted-foreground">
                <tr className="border-t border-border">
                  <th className="px-[18px] py-2 font-medium">Key</th>
                  <th className="py-2 pr-3 font-medium">Why</th>
                  <th className="py-2 pr-3 font-medium">Address</th>
                  <th className="py-2 pr-3 font-medium">Country</th>
                  <th className="py-2 pr-3 text-right font-medium">Refused</th>
                  <th className="py-2 pr-3 text-right font-medium">Last</th>
                  <th className="py-2 pr-[18px] font-medium">User agent</th>
                </tr>
              </thead>
              <tbody>
                {refusals.slice(0, refusalsShown).map((r) => (
                  <tr key={`${r.key_id}:${r.reason}:${r.client_ip}:${r.client_country}`} className="border-t border-border">
                    <td className="px-[18px] py-[9px]">
                      <Link href={`/keys?key=${r.key_id}`} className="font-medium underline-offset-2 hover:underline">{r.key_name || `${r.key_id.slice(0, 8)}…`}</Link>
                      <span className="block text-xs text-muted-foreground">{r.tenant_name}</span>
                    </td>
                    <td className="py-[9px] pr-3">{REFUSAL_REASON[r.reason] ?? r.reason}</td>
                    <td className="py-[9px] pr-3">
                      <span className="block font-mono text-xs">{r.client_ip || "—"}</span>
                      {r.network_name && <span className="mt-0.5 block text-xs text-muted-foreground">{r.network_name}</span>}
                    </td>
                    <td className={cn("py-[9px] pr-3", !r.client_country && "text-muted-foreground")}>{placeLabel(r.client_country, r.client_ip)}</td>
                    <td className="py-[9px] pr-3 text-right font-mono text-xs tabular-nums">{formatNumber(r.count)}</td>
                    <td className="py-[9px] pr-3 text-right text-xs text-muted-foreground" title={`First ${fullDateTime(r.first_seen_ms)}, last ${fullDateTime(r.last_seen_ms)}`}>{ago(r.last_seen_ms, now)}</td>
                    <td className="max-w-[18rem] truncate py-[9px] pr-[18px] text-xs text-muted-foreground" title={r.sample_user_agent}>{r.sample_user_agent || "—"}</td>
                  </tr>
                ))}
              </tbody>
            </table>
            <TableFooter total={refusals.length} shown={refusalsShown} noun="refusals" onMore={() => setRefusalsShown(refusalsShown + PAGE)} />
          </div>
        )}
      </Panel>
    </div>
  );
}

/** A table paged in the browser: how many of its rows show, and the next page. */
function TableFooter({ total, shown, noun, onMore }: { total: number; shown: number; noun: string; onMore: () => void }) {
  return <ListFooter shown={Math.min(shown, total)} total={total} noun={noun} more={total > shown} onMore={onMore} action={`Show ${Math.min(PAGE, total - shown)} more`} />;
}
