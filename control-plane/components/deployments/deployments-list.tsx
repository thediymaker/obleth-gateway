"use client";

import { useMemo, useState } from "react";
import Link from "next/link";
import { usePathname, useRouter } from "next/navigation";
import { useQuery } from "@tanstack/react-query";
import { Plus, Search } from "lucide-react";
import { RecipesTab } from "@/components/deployments/recipes-tab";
import { SlurmConnection } from "@/components/deployments/slurm-connection";
import { Glyph, ReplicaDots, StateMark } from "@/components/deployments/ui";
import { useFairshareLive } from "@/components/fairshare/hooks";
import { Tile } from "@/components/models/ui";
import { Meter, Pill, Segmented } from "@/components/overview/ui";
import { Button } from "@/components/ui/button";
import { Select } from "@/components/ui/select";
import type { RecipeCard } from "@/components/recipes/recipe-card";
import type { DeploymentsData } from "@/lib/deployments-data";
import {
  buildDeploymentRows,
  countReplicas,
  deploymentHref,
  deploymentsLine,
  filterDeployments,
  nodeState,
  sortDeployments,
  type DeploymentRow,
  type DeploySort,
  type RunsFilter,
} from "@/lib/deployments-model";
import { logsHref } from "@/lib/log-links";
import { MODEL_TYPE_NAMES } from "@/lib/models-model";
import { compact } from "@/lib/overview-model";
import { useClusterResources } from "@/lib/use-cluster-resources";
import { cn, getJson } from "@/lib/utils";

const COLS = "grid-cols-[minmax(0,1.4fr)_minmax(130px,0.9fr)_150px_minmax(0,1.4fr)_130px_84px_84px]";

function ago(secs: number | null | undefined): string {
  if (secs == null) return "never";
  if (secs < 60) return `${Math.round(secs)} s ago`;
  if (secs < 3600) return `${Math.round(secs / 60)} min ago`;
  return `${Math.round(secs / 3600)} h ago`;
}

function inFlightOf(r: DeploymentRow, live: Record<string, number> | undefined): number {
  if (r.discovery) return r.discovery.cluster_in_flight ?? r.discovery.in_flight ?? 0;
  return live?.[r.model.model_name] ?? 0;
}

export function DeploymentsList({ initial, recipes, tab: initialTab, slurmSheet = false }: { initial: DeploymentsData; recipes: RecipeCard[]; tab: "deployments" | "recipes"; slurmSheet?: boolean }) {
  const router = useRouter();
  const pathname = usePathname();
  const [tab, setTab] = useState(initialTab);
  const [slurmOpen, setSlurmOpen] = useState(slurmSheet);
  const [query, setQuery] = useState("");
  const [runs, setRuns] = useState<RunsFilter>("all");
  const [sort, setSort] = useState<DeploySort>("requests");
  const live = useQuery({ queryKey: ["deployments"], queryFn: () => getJson<DeploymentsData>("/api/live/deployments"), initialData: initial, refetchInterval: 15_000 });
  const fairshare = useFairshareLive();
  const data = live.data ?? initial;
  const slurmOn = !!data.slurm?.enabled;
  const resources = useClusterResources();

  const rows = useMemo(
    () => buildDeploymentRows(data.models, data.specs, data.replicas, data.discovery?.models ?? [], data.requests),
    [data],
  );
  const shown = useMemo(() => sortDeployments(filterDeployments(rows, { query, runs }), sort), [rows, query, runs, sort]);
  const k8s = rows.filter((r) => r.kind === "kubernetes");
  const slurm = rows.filter((r) => r.kind === "slurm");
  const attention = rows.filter((r) => r.view.attention);
  const flights = fairshare.data?.model_in_flight;
  const full = rows.filter((r) => r.poolCap > 0 && inFlightOf(r, flights) >= r.poolCap);
  const busiest = [...rows].filter((r) => r.poolCap > 0).sort((a, b) => inFlightOf(b, flights) / b.poolCap - inFlightOf(a, flights) / a.poolCap)[0];
  const readyTotal = rows.reduce((n, r) => n + r.ready, 0);
  const slurmWanted = slurm.reduce((n, r) => n + (r.wanted ?? 0), 0);
  const slurmReady = slurm.reduce((n, r) => n + r.ready, 0);
  const queued = slurm.reduce((n, r) => { const c = countReplicas(r.replicas); return n + c.queued + c.running; }, 0);
  const namespaces = new Map<string, number>();
  for (const r of k8s) { const ns = r.discovery?.status?.namespace; if (ns) namespaces.set(ns, (namespaces.get(ns) ?? 0) + 1); }
  const nodeCounts = { idle: 0, busy: 0, down: 0 };
  for (const n of resources.nodes) nodeCounts[nodeState(n.state)] += 1;
  const nodesTotal = resources.nodes.length;

  const switchTab = (t: "deployments" | "recipes") => {
    setTab(t);
    router.replace(t === "recipes" ? `${pathname}?tab=recipes` : pathname, { scroll: false });
  };

  function closeSlurm() {
    setSlurmOpen(false);
    if (slurmSheet) router.replace(tab === "recipes" ? `${pathname}?tab=recipes` : pathname, { scroll: false });
  }

  return (
    <div className="mx-auto flex max-w-[1600px] flex-col gap-[18px]">
      {slurmOpen && data.slurm && <SlurmConnection settings={data.slurm} replicas={queued + slurmReady} onClose={closeSlurm} onSaved={() => { void live.refetch(); router.refresh(); }} />}
      <div className="flex flex-wrap items-end justify-between gap-4">
        <div className="flex min-w-0 flex-col gap-1.5">
          <h1 className="text-[26px] font-semibold tracking-tight">Deployments</h1>
          <p className="text-[13px] text-secondary-foreground">{tab === "recipes" ? `${recipes.length} recipe${recipes.length === 1 ? "" : "s"}: ${recipes.filter((r) => r.source === "file").length} shipped as files, ${recipes.filter((r) => r.source === "db").length} saved here` : deploymentsLine(rows)}</p>
        </div>
        <div className="flex gap-2">
          {tab === "deployments" && <Button type="button" variant="outline" size="sm" className="h-9" onClick={() => switchTab("recipes")}>Recipes</Button>}
          <Button asChild size="sm" className="h-9"><Link href="/deployments/new"><Plus className="h-4 w-4" />New deployment</Link></Button>
        </div>
      </div>

      <div role="tablist" aria-label="Deployments" className="flex gap-1 border-b border-border">
        {(["deployments", "recipes"] as const).map((t) => (
          <button
            key={t}
            type="button"
            role="tab"
            aria-selected={tab === t}
            onClick={() => switchTab(t)}
            className={cn("-mb-px border-b-2 px-3 py-2.5 text-[13.5px]", tab === t ? "border-foreground text-foreground" : "border-transparent text-muted-foreground hover:text-foreground")}
          >
            {t === "deployments" ? `Deployments · ${rows.length}` : `Recipes · ${recipes.length}`}
          </button>
        ))}
      </div>

      {tab === "recipes" ? (
        <RecipesTab recipes={recipes} specs={data.specs} models={data.models} />
      ) : (
        <>
          <div className="grid gap-3 lg:grid-cols-2">
            <section aria-label="Kubernetes" className="flex flex-col gap-2.5 rounded-xl border border-border bg-card px-4 py-3.5">
              <div className="flex flex-wrap items-center justify-between gap-2">
                <span className="flex items-center gap-2.5"><span className="text-sm font-semibold">Kubernetes</span><Pill>watched</Pill></span>
                <span className="inline-flex items-center gap-2 text-xs text-secondary-foreground">
                  <Glyph glyph={data.discovery?.enabled ? "on" : "off"} className="h-[7px] w-[7px]" />
                  {data.discovery?.enabled ? `Discovery every ${data.discovery.interval_secs} s` : "Discovery is off"}
                </span>
              </div>
              <p className="text-[12.5px] text-secondary-foreground">The cluster starts and scales these. obleth reads each model&apos;s Service and sizes its pool from the ready replicas.</p>
              <div className="flex flex-wrap gap-x-5 gap-y-1 text-xs text-muted-foreground">
                {[...namespaces.entries()].map(([ns, n]) => <span key={ns}><span className="font-mono text-foreground">{ns}</span> · {n} model{n === 1 ? "" : "s"}</span>)}
                {namespaces.size === 0 && <span>{data.discovery?.enabled ? "No model uses Kubernetes discovery yet." : "Turn it on with OBLETH_CAPACITY_DISCOVERY_ENABLED."}</span>}
              </div>
            </section>
            <section aria-label="Slurm" className="flex flex-col gap-2.5 rounded-xl border border-border bg-card px-4 py-3.5">
              <div className="flex flex-wrap items-center justify-between gap-2">
                <span className="flex items-center gap-2.5"><span className="text-sm font-semibold">Slurm</span><Pill>launched by obleth</Pill></span>
                {slurmOn && (
                  <span className="inline-flex items-center gap-2 text-xs text-secondary-foreground">
                    <Glyph glyph={data.slurm?.provisioner_running ? (data.slurm.provisioner_tick_status === "error" ? "half" : "on") : "off"} className="h-[7px] w-[7px]" />
                    {data.slurm?.provisioner_running ? `Provisioner checked in ${ago(data.slurm.provisioner_last_seen_secs)}${data.slurm.provisioner_tick_status === "error" ? " · its last pass failed" : ""}` : "Provisioner not running"}
                  </span>
                )}
              </div>
              {slurmOn ? (
                <>
                  {nodesTotal > 0 ? (
                    <div className="flex flex-col gap-1.5">
                      <div className="flex h-2 gap-0.5 overflow-hidden rounded-[3px] bg-muted" aria-hidden="true">
                        <span className="bg-foreground" style={{ width: `${(nodeCounts.idle / nodesTotal) * 100}%` }} />
                        <span className="bg-muted-foreground/60" style={{ width: `${(nodeCounts.busy / nodesTotal) * 100}%` }} />
                        <span className="border border-border" style={{ width: `${(nodeCounts.down / nodesTotal) * 100}%` }} />
                      </div>
                      <span className="text-xs text-muted-foreground">
                        {nodesTotal} nodes in {resources.partitions.length} partitions · {nodeCounts.idle} idle · {nodeCounts.busy} busy{nodeCounts.down ? ` · ${nodeCounts.down} down or draining` : ""} · {data.slurm?.slurmrestd_api_version}
                      </span>
                    </div>
                  ) : (
                    <p className="text-[12.5px] text-secondary-foreground">Connected to {data.slurm?.slurmrestd_url || "slurmrestd"}. The cluster&apos;s partitions and nodes will show here.</p>
                  )}
                  <p className="text-xs text-muted-foreground">{slurm.length ? `${slurmReady} of ${slurmWanted} replicas serving${queued ? ` · ${queued} starting` : ""}` : "Nothing launched yet."} <button type="button" onClick={() => setSlurmOpen(true)} className="text-secondary-foreground underline underline-offset-2 hover:text-foreground">Slurm connection</button></p>
                </>
              ) : (
                <p className="text-[12.5px] text-secondary-foreground">
                  obleth can launch models as jobs on a Slurm cluster, keep them at a replica count, and route to them once healthy.{" "}
                  <button type="button" onClick={() => setSlurmOpen(true)} disabled={!data.slurm} className="text-foreground underline underline-offset-2 disabled:no-underline disabled:text-muted-foreground">{data.slurm ? "Set up Slurm ›" : "Slurm settings couldn't be read"}</button>
                </p>
              )}
            </section>
          </div>

          <div className="grid grid-cols-2 gap-3 xl:grid-cols-4">
            <Tile label="Deployments" value={rows.length} detail={`${k8s.length} Kubernetes · ${slurm.length} Slurm`} />
            <Tile label="Replicas ready" value={compact(readyTotal)} detail={slurm.length ? `${slurmReady} of ${slurmWanted} on Slurm${queued ? ` · ${queued} starting` : ""}` : `across ${k8s.length} Services`} />
            <Tile
              label="Needs you"
              value={attention.length}
              emphasis={attention.length > 0}
              pressed={runs === "attention"}
              onClick={attention.length ? () => setRuns(runs === "attention" ? "all" : "attention") : undefined}
              detail={attention.length ? attention.map((r) => `${r.model.model_name} ${r.view.state === "stopped" ? "stopped" : "has no replicas"}`).join(" · ") : "Nothing stopped or empty"}
            />
            <Tile
              label="Pools full now"
              value={full.length}
              emphasis={full.length > 0}
              detail={full.length ? full.map((r) => r.model.model_name).join(" · ") : busiest ? `${busiest.model.model_name} busiest, ${inFlightOf(busiest, flights)} of ${busiest.poolCap}` : null}
            />
          </div>

          <div className="flex flex-wrap items-center gap-2">
            <label className="flex h-9 min-w-[240px] max-w-[380px] flex-1 items-center gap-2 rounded-lg border border-border bg-background px-3 text-[13px] focus-within:border-muted-foreground">
              <Search className="h-4 w-4 shrink-0 text-muted-foreground" aria-hidden />
              <input value={query} onChange={(e) => setQuery(e.target.value)} aria-label="Search deployments" placeholder="Model, Service, partition or job" className="min-w-0 flex-1 bg-transparent outline-none placeholder:text-muted-foreground" />
            </label>
            <Segmented
              label="Runs on"
              value={runs}
              onChange={setRuns}
              options={[
                { value: "all", label: `All ${rows.length}` },
                { value: "kubernetes", label: `Kubernetes ${k8s.length}` },
                { value: "slurm", label: `Slurm ${slurm.length}` },
                { value: "attention", label: `Needs you ${attention.length}` },
              ]}
            />
            <Select aria-label="Sort" value={sort} onValueChange={(v) => setSort(v as DeploySort)} className="h-9 w-auto min-w-[10rem] text-[12.5px]" options={[{ value: "requests", label: "Sort: most requests" }, { value: "attention", label: "Sort: problems first" }, { value: "replicas", label: "Sort: most replicas" }, { value: "name", label: "Sort: name" }]} />
          </div>

          <section aria-label="Deployments" className="overflow-hidden rounded-xl border border-border bg-card">
            <div className="overflow-x-auto">
              <div className="min-w-[1000px]">
                <div className={cn("grid items-center gap-3.5 px-[18px] py-2.5 text-[11px] font-semibold uppercase tracking-[0.07em] text-muted-foreground", COLS)}>
                  <span>Model</span><span>Status</span><span>Replicas</span><span>Runs on</span><span>Pool now</span><span className="text-right">Req · 24h</span><span className="text-right">Per replica</span>
                </div>
                {shown.length === 0 && (
                  <p className="border-t border-border px-[18px] py-8 text-center text-[13px] text-muted-foreground">
                    {rows.length ? "Nothing matches these filters." : <>Nothing deployed yet. <Link href="/deployments/new" className="text-foreground underline underline-offset-2">Launch a recipe</Link>{" "}or point a model&apos;s capacity at a Kubernetes Service.</>}
                  </p>
                )}
                {shown.map((r) => {
                  const c = countReplicas(r.replicas);
                  const inFlight = inFlightOf(r, flights);
                  const href = deploymentHref(r.model.model_name);
                  return (
                    <div
                      key={r.model.id}
                      onClick={(e) => { if (e.target instanceof Element && e.target.closest("a, button")) return; router.push(href); }}
                      className={cn("grid min-h-[52px] cursor-pointer items-center gap-3.5 border-t border-border px-[18px] py-2 text-[13px] hover:bg-muted/30", COLS)}
                    >
                      <span className="flex min-w-0 flex-col">
                        <Link href={href} className="truncate font-mono text-[12.5px] font-medium text-foreground hover:underline">{r.model.model_name}</Link>
                        <span className="truncate text-[11.5px] text-muted-foreground">{[r.engine, MODEL_TYPE_NAMES[r.model.model_type]?.toLowerCase() ?? r.model.model_type].filter(Boolean).join(" · ")}</span>
                      </span>
                      <span className="flex min-w-0 flex-col items-start gap-0.5">
                        <StateMark state={r.view.state} />
                        {r.view.why && <span className="max-w-full truncate text-[11.5px] text-muted-foreground" title={r.view.why}>{r.view.why}</span>}
                      </span>
                      <span className="flex items-center gap-2">
                        <ReplicaDots ready={r.ready} starting={c.queued + c.running} wanted={r.wanted} />
                        <span className="font-mono text-[12px] text-secondary-foreground">{r.kind === "slurm" ? `${r.ready} / ${r.wanted}` : r.ready}</span>
                      </span>
                      <span className="flex min-w-0 flex-col">
                        <span className="text-[12.5px]">{r.kind === "slurm" ? "Slurm" : "Kubernetes"}</span>
                        <span className="truncate font-mono text-[11.5px] text-muted-foreground" title={r.where}>{r.where}</span>
                      </span>
                      <span className="flex flex-col gap-1">
                        <span className="font-mono text-[12px]">{r.poolCap ? `${inFlight} of ${r.poolCap}` : "—"}</span>
                        <Meter value={inFlight} max={r.poolCap} strong={r.poolCap > 0 && inFlight >= r.poolCap} />
                      </span>
                      {r.requests24h ? (
                        <Link href={logsHref({ model: r.model.model_name, window: "24h" })} title="See these requests" className="text-right font-mono text-[12px] underline decoration-muted-foreground/40 underline-offset-[3px] hover:decoration-foreground">{compact(r.requests24h)}</Link>
                      ) : (
                        <span className="text-right font-mono text-[12px] text-muted-foreground">0</span>
                      )}
                      <span className="text-right font-mono text-[12px] text-secondary-foreground">{r.perReplica ?? (r.kind === "slurm" && r.wanted ? Math.round(r.poolCap / Math.max(1, r.wanted)) || "—" : "—")}</span>
                    </div>
                  );
                })}
              </div>
            </div>
            <div className="flex flex-wrap justify-between gap-2 border-t border-border px-[18px] py-3 text-[12.5px] text-muted-foreground">
              <span>{shown.length === rows.length ? `${rows.length} deployments` : `${shown.length} of ${rows.length} deployments`} · click one for its page</span>
              <span>Filled dot serving · half dot starting · dashed not running · white pill needs you</span>
            </div>
          </section>
        </>
      )}
    </div>
  );
}
