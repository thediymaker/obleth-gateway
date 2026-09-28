"use client";

import { useCallback, useEffect, useMemo, useRef, useState, useTransition } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { MoreHorizontal, Search } from "lucide-react";
import { activateModelAction, checkModelHealthAction, deleteModelAction, setModelEnabledAction } from "@/app/actions";
import { useCapacityDiscovery, useFairshareLive } from "@/components/fairshare/hooks";
import { EndpointsSection } from "@/components/models/endpoints";
import { Switch } from "@/components/models/fields";
import { ModelOverview } from "@/components/models/model-overview";
import { ModelSettings, type SettingsState } from "@/components/models/model-settings";
import { Notice, ProviderMark, StatusMark } from "@/components/models/ui";
import { Pill } from "@/components/overview/ui";
import { Button } from "@/components/ui/button";
import { useConfirm } from "@/components/ui/confirm-dialog";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuSeparator, DropdownMenuTrigger } from "@/components/ui/dropdown-menu";
import type { BoonBlockers } from "@/lib/boon-availability";
import { deploymentHref } from "@/lib/deployments-model";
import {
  awaitingFirstPass,
  contextLabel,
  MODEL_TYPE_NAMES,
  modelStatus,
  RUNS_LABELS,
  runsOn,
  searchSettings,
  SECTION_LABEL,
  type ModelOverviewData,
  type SettingEntry,
  type SettingsSectionId,
} from "@/lib/models-model";
import type { McpServer, ModelEndpoint, ModelHealthDetail, ModelHealthSummary, ModelRoute } from "@/lib/obleth";
import { poolOccupancy } from "@/lib/overview-model";
import { cn, getJson } from "@/lib/utils";

type NavId = "overview" | SettingsSectionId | "activity";

function flash(el: HTMLElement) {
  el.classList.remove("setting-flash");
  // Restart the animation when the same setting is found twice in a row.
  void el.offsetWidth;
  el.classList.add("setting-flash");
  setTimeout(() => el.classList.remove("setting-flash"), 1800);
}

/** Scroll to a setting, mark it for a moment, and put the cursor in its first field. */
function jumpTo(id: string) {
  const el = document.getElementById(id);
  if (!el) return;
  el.scrollIntoView({ behavior: "smooth", block: id.startsWith("set-") ? "center" : "start" });
  if (id.startsWith("set-")) {
    flash(el);
    el.querySelector<HTMLElement>("input:not([type=hidden]):not([disabled]), textarea, button[role=switch], [role=combobox], button")?.focus({ preventScroll: true });
  }
}

function FindSetting({ modelType }: { modelType: string }) {
  const [query, setQuery] = useState("");
  const [open, setOpen] = useState(false);
  const [index, setIndex] = useState(0);
  const input = useRef<HTMLInputElement>(null);
  // Only settings this page has rendered: Routing, for one, is chat-only.
  const results = useMemo(
    () => (typeof document === "undefined" ? [] : searchSettings(query, modelType).filter((r) => document.getElementById(r.id))),
    [query, modelType],
  );

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        input.current?.focus();
        input.current?.select();
      }
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, []);

  const choose = (r: SettingEntry) => {
    setOpen(false);
    setQuery("");
    input.current?.blur();
    jumpTo(r.id);
  };

  return (
    <div className="relative">
      <label className="flex h-9 items-center gap-2 rounded-lg border border-border bg-background px-2.5 text-[12.5px] focus-within:border-muted-foreground">
        <Search className="h-3.5 w-3.5 shrink-0 text-muted-foreground" aria-hidden />
        <input
          ref={input}
          value={query}
          onChange={(e) => { setQuery(e.target.value); setIndex(0); setOpen(true); }}
          onFocus={() => setOpen(true)}
          onBlur={() => setTimeout(() => setOpen(false), 120)}
          onKeyDown={(e) => {
            if (e.key === "ArrowDown") { e.preventDefault(); setIndex((i) => Math.min(i + 1, results.length - 1)); }
            if (e.key === "ArrowUp") { e.preventDefault(); setIndex((i) => Math.max(i - 1, 0)); }
            if (e.key === "Enter" && results[index]) { e.preventDefault(); choose(results[index]); }
            if (e.key === "Escape") { setQuery(""); input.current?.blur(); }
          }}
          placeholder="Find a setting"
          aria-label="Find a setting"
          role="combobox"
          aria-expanded={open && results.length > 0}
          aria-controls="setting-results"
          className="min-w-0 flex-1 bg-transparent outline-none placeholder:text-muted-foreground"
        />
        <kbd className="rounded border border-border px-1 font-mono text-[10.5px] text-muted-foreground">⌘K</kbd>
      </label>
      {open && query.trim() !== "" && (
        <div id="setting-results" role="listbox" aria-label="Matching settings" className="absolute left-0 top-10 z-40 w-[min(340px,calc(100vw-2rem))] rounded-[10px] border border-border bg-[hsl(240_5%_9%)] p-1.5 shadow-2xl">
          {results.length === 0 ? (
            <p className="px-2.5 py-2 text-xs text-muted-foreground">No setting matches “{query}”.</p>
          ) : (
            results.map((r, i) => (
              <button
                key={r.id}
                type="button"
                role="option"
                aria-selected={i === index}
                onMouseDown={(e) => e.preventDefault()}
                onClick={() => choose(r)}
                onMouseEnter={() => setIndex(i)}
                className={cn("flex w-full items-center justify-between gap-3 rounded-md px-2.5 py-2 text-left text-[13px]", i === index && "bg-secondary")}
              >
                <span className="truncate">{r.label}</span>
                <span className="shrink-0 font-mono text-[11px] text-muted-foreground">{SECTION_LABEL[r.section]}</span>
              </button>
            ))
          )}
          {results.length > 0 && <p className="mt-1 border-t border-border px-2.5 pb-0.5 pt-1.5 text-[11px] text-muted-foreground">↑↓ to move · ↵ to jump</p>}
        </div>
      )}
    </div>
  );
}

export function ModelPage({
  model,
  summary: initialSummary,
  managed,
  mcpServers,
  modelNames,
  boonBlockers,
}: {
  model: ModelRoute;
  summary: ModelHealthSummary;
  managed: boolean;
  mcpServers: McpServer[];
  modelNames: string[];
  boonBlockers: BoonBlockers;
}) {
  const router = useRouter();
  const queryClient = useQueryClient();
  const { confirm, confirmElement } = useConfirm();
  const [pending, start] = useTransition();
  const [notice, setNotice] = useState<{ text: string; strong?: boolean } | null>(null);
  const [settings, setSettings] = useState<SettingsState>({ dirty: [], count: 0 });
  const [active, setActive] = useState<NavId>("overview");

  const detailQuery = useQuery({
    queryKey: ["model-health-detail", model.id],
    queryFn: () => getJson<ModelHealthDetail>(`/api/live/models/${model.id}/health`),
    refetchInterval: 20_000,
  });
  const endpointsQuery = useQuery({
    queryKey: ["model-endpoints", model.id],
    queryFn: () => getJson<ModelEndpoint[]>(`/api/live/models/${model.id}/endpoints`),
    refetchInterval: 30_000,
  });
  const overviewQuery = useQuery({
    queryKey: ["model-overview", model.id],
    queryFn: () => getJson<ModelOverviewData>(`/api/live/models/${model.id}/overview?name=${encodeURIComponent(model.model_name)}`),
    refetchInterval: 60_000,
  });
  const fairshare = useFairshareLive();
  const discovery = useCapacityDiscovery();

  const summary = detailQuery.data?.summary ?? initialSummary;
  const checks = detailQuery.data?.checks ?? [];
  const endpoints = endpointsQuery.data ?? [];
  const status = modelStatus(model, summary);
  const runs = runsOn(model, managed);
  const load = model.enabled ? poolOccupancy(model.model_name, model, fairshare.data, discovery.data) : { inFlight: 0, cap: 0, queued: 0 };
  const capacityStatus = discovery.data?.models.find((m) => m.model_id === model.id)?.status;
  const first = awaitingFirstPass(checks);
  const chat = model.model_type === "chat";
  const textual = chat || model.model_type === "embedding";

  const refreshHealth = useCallback(async () => {
    await queryClient.invalidateQueries({ queryKey: ["model-health-detail", model.id] });
    router.refresh();
  }, [queryClient, model.id, router]);

  function setServing(on: boolean) {
    start(async () => {
      if (!on) {
        const ok = await confirm({
          title: `Turn off ${model.model_name}?`,
          description: "Requests naming it start failing straight away. Its settings are kept.",
          confirmLabel: "Turn off",
        });
        if (!ok) return;
      }
      const result = await setModelEnabledAction(model.id, on);
      setNotice(result.ok ? null : { text: result.error, strong: true });
      await refreshHealth();
    });
  }

  function check() {
    start(async () => {
      if (model.enabled) {
        const result = await checkModelHealthAction(model.id);
        setNotice(result.ok ? null : { text: result.error, strong: true });
      } else {
        const result = await activateModelAction(model.id);
        if (!result.ok) setNotice({ text: result.error, strong: true });
        else if (result.activated) setNotice({ text: "The check passed, so the model is on and serving." });
        else setNotice({ text: `Still off: the check came back ${result.status}.${result.message ? ` ${result.message}` : ""}`, strong: true });
      }
      await refreshHealth();
    });
  }

  function remove() {
    start(async () => {
      const ok = await confirm({
        title: `Delete ${model.model_name}?`,
        description: "Requests naming it start failing immediately. This cannot be undone.",
        confirmLabel: "Delete",
      });
      if (!ok) return;
      const result = await deleteModelAction(model.id);
      if (result.ok) router.push("/models");
      else setNotice({ text: `Delete failed: ${result.error}`, strong: true });
    });
  }

  // Which part of the page is on screen, for the section list.
  useEffect(() => {
    const ids: NavId[] = ["overview", "general", "connection", "routing", "capabilities", "pricing", "capacity", "health", "endpoints", "deployment", "activity"];
    const els = ids.map((id) => document.getElementById(id)).filter((e): e is HTMLElement => !!e);
    const observer = new IntersectionObserver(
      (entries) => {
        const visible = entries.filter((e) => e.isIntersecting).sort((a, b) => a.boundingClientRect.top - b.boundingClientRect.top);
        if (visible[0]) setActive(visible[0].target.id as NavId);
      },
      { rootMargin: "-20% 0px -65% 0px" },
    );
    els.forEach((e) => observer.observe(e));
    return () => observer.disconnect();
  }, []);

  // Arriving with a #section (the list's links, the Overview's "Edit" links).
  useEffect(() => {
    const id = window.location.hash.slice(1);
    if (id) requestAnimationFrame(() => jumpTo(id));
  }, []);

  const tagCount = (model.tags ?? []).length;
  const nativeOn = [model.supports_function_calling, model.supports_tool_choice, model.supports_response_schema, model.supports_system_messages].filter(Boolean).length;
  const nav: { id: NavId; label: string; count?: string | number; show?: boolean }[] = [
    { id: "overview", label: "Overview" },
    { id: "general", label: "General" },
    { id: "connection", label: "Connection" },
    { id: "routing", label: "Routing", count: tagCount ? `${tagCount} tag${tagCount === 1 ? "" : "s"}` : undefined, show: chat },
    { id: "capabilities", label: "Capabilities", count: chat ? `${nativeOn} on` : undefined, show: textual },
    { id: "pricing", label: "Pricing" },
    { id: "capacity", label: "Capacity", count: load.cap || undefined },
    { id: "health", label: "Health" },
    { id: "endpoints", label: "Endpoints", count: endpoints.length || undefined },
    { id: "deployment", label: "Deployment" },
  ];
  const onSettings = useCallback((s: SettingsState) => setSettings(s), []);

  return (
    <div className="mx-auto flex max-w-[1600px] flex-col gap-5">
      {confirmElement}
      <div className="flex flex-wrap items-end justify-between gap-4">
        <div className="flex min-w-0 flex-col gap-1.5">
          <p className="text-[12.5px] text-muted-foreground"><Link href="/models" className="text-secondary-foreground hover:text-foreground">Models</Link> / {MODEL_TYPE_NAMES[model.model_type]?.toLowerCase() ?? model.model_type}</p>
          <h1 className="flex min-w-0 items-center gap-3 text-[26px] font-semibold tracking-tight">
            <ProviderMark name={model.model_name} upstream={model.upstream_model} size={30} />
            <span className="truncate">{model.model_name}</span>
          </h1>
          <div className="flex flex-wrap items-center gap-2">
            {status === "down" ? <Pill inverted>Failing health checks</Pill> : <Pill><StatusMark status={status} /></Pill>}
            <Pill>{MODEL_TYPE_NAMES[model.model_type] ?? model.model_type}{textual ? ` · ${contextLabel(model.context_window)} context` : ""}</Pill>
            <Pill>{model.capacity_mode === "discovered" ? "Discovered capacity" : model.capacity_mode === "tuned" ? "Tuned capacity" : model.max_in_flight ? `${model.max_in_flight} slots` : "No slot cap"}</Pill>
            {chat && <Pill>{model.auto_eligible ? "Eligible for auto" : "Not picked by auto"}</Pill>}
            <Pill>{RUNS_LABELS[runs]}</Pill>
            {(model.aliases?.length ?? 0) > 0 && <span className="text-[12.5px] text-muted-foreground">also answers to <span className="font-mono">{model.aliases.join(", ")}</span></span>}
          </div>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <div className="inline-flex h-9 items-center gap-2.5 rounded-lg border border-border px-3 text-[13px]">
            <Switch label="Serving" checked={model.enabled} disabled={pending} onChange={setServing} />
            Serving
          </div>
          <Button type="button" variant="outline" size="sm" className="h-9" disabled={pending} onClick={check}>
            {model.enabled ? "Check health now" : "Check and turn on"}
          </Button>
          {model.enabled && (
            <Button asChild variant="outline" size="sm" className="h-9">
              <Link href={`/playground?model=${encodeURIComponent(model.model_name)}`}>Try in Playground</Link>
            </Button>
          )}
          <DropdownMenu>
            <DropdownMenuTrigger asChild>
              <Button type="button" variant="outline" size="icon" className="h-9 w-9" aria-label="More actions"><MoreHorizontal className="h-4 w-4" /></Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end">
              <DropdownMenuItem onSelect={() => { window.location.href = `/api/live/models/export?names=${encodeURIComponent(model.model_name)}`; }}>Export this model</DropdownMenuItem>
              <DropdownMenuItem onSelect={() => { void navigator.clipboard?.writeText(model.model_name); }}>Copy the model name</DropdownMenuItem>
              <DropdownMenuSeparator />
              <DropdownMenuItem onSelect={remove}>Delete model…</DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        </div>
      </div>

      {!model.enabled && first.waiting && (
        <Notice strong>
          <p className="font-medium">Off until a health check passes.</p>
          <p className="mt-0.5 text-xs text-muted-foreground">
            {first.last ? <>The last check came back {first.last.status}{first.last.message ? `: ${first.last.message}` : "."} Fix the connection below, then check again.</> : "It has not been checked yet."}
          </p>
          <Button type="button" size="sm" className="mt-2" disabled={pending} onClick={check}>Check and turn on</Button>
        </Notice>
      )}
      {notice && <Notice onDismiss={() => setNotice(null)} strong={notice.strong}>{notice.text}</Notice>}

      <div className="grid items-start gap-6 lg:grid-cols-[200px_minmax(0,1fr)]">
        <nav aria-label="Model sections" className="flex flex-col gap-0.5 lg:sticky lg:top-0">
          <div className="mb-2"><FindSetting modelType={model.model_type} /></div>
          {nav.filter((n) => n.show !== false).map((n) => (
            <a
              key={n.id}
              href={`#${n.id}`}
              onClick={(e) => { e.preventDefault(); jumpTo(n.id); history.replaceState(null, "", `#${n.id}`); }}
              aria-current={active === n.id ? "true" : undefined}
              className={cn(
                "flex h-8 items-center justify-between rounded-lg px-2.5 text-[13px] transition-colors",
                active === n.id ? "bg-secondary text-foreground" : "text-muted-foreground hover:bg-muted/40 hover:text-foreground",
              )}
            >
              {n.label}
              <span className="flex items-center gap-1.5 text-[11.5px] text-muted-foreground">
                {n.count}
                {settings.dirty.includes(n.id as SettingsSectionId) && <span aria-label="unsaved changes" className="h-1.5 w-1.5 rounded-full bg-foreground" />}
              </span>
            </a>
          ))}
          <div className="mx-1 my-2 h-px bg-border" />
          <a href="#activity" onClick={(e) => { e.preventDefault(); jumpTo("activity"); }} className={cn("flex h-8 items-center rounded-lg px-2.5 text-[13px] transition-colors", active === "activity" ? "bg-secondary text-foreground" : "text-muted-foreground hover:bg-muted/40 hover:text-foreground")}>
            Activity
          </a>
        </nav>

        <div className="flex min-w-0 flex-col gap-4">
          <div id="overview" className="scroll-mt-24">
            <ModelOverview model={model} data={overviewQuery.data} summary={summary} checks={checks} runs={runs} capacity={capacityStatus} load={load} endpoints={endpoints.length} />
          </div>
          <ModelSettings
            model={model}
            summary={summary}
            checks={checks}
            mcpServers={mcpServers}
            modelNames={modelNames}
            boonBlockers={boonBlockers}
            load={load}
            onStateChange={onSettings}
          />
          <EndpointsSection model={model} endpoints={endpoints} onChanged={() => router.refresh()} />
          <DeploymentSection model={model} managed={managed} runs={runs} />
        </div>
      </div>
    </div>
  );
}

function DeploymentSection({ model, managed, runs }: { model: ModelRoute; managed: boolean; runs: ReturnType<typeof runsOn> }) {
  const watched = runs === "kubernetes";
  return (
    <section id="deployment" data-section="deployment" aria-label="Deployment" className="scroll-mt-24 rounded-xl border border-border bg-card">
      <header className="flex flex-wrap items-start justify-between gap-3 px-[18px] pb-3 pt-4">
        <div className="min-w-0">
          <h2 className="text-sm font-semibold">Deployment</h2>
          <p className="mt-0.5 text-xs text-muted-foreground">Where the replicas come from.</p>
        </div>
        {(managed || watched) && (
          <Button asChild variant="outline" size="sm"><Link href={deploymentHref(model.model_name)}>Open its deployment ›</Link></Button>
        )}
      </header>
      <div className="border-t border-border px-[18px] py-4">
        {managed ? (
          <p className="text-[13px] text-secondary-foreground">obleth launches this model as jobs on Slurm and keeps it at its replica count. Its replicas, script and placement are on its deployment page.</p>
        ) : watched ? (
          <p className="text-[13px] text-secondary-foreground">
            Runs on Kubernetes. The cluster starts and scales the replicas; obleth counts the ready ones behind{" "}
            <span className="font-mono text-[12px]">{model.capacity_service || "the default Service"}</span> and sizes the pool to match (see <a href="#set-discovery" className="underline underline-offset-2">Capacity</a>).
          </p>
        ) : (
          <p className="text-[13px] text-secondary-foreground">
            An endpoint obleth connects to. It doesn&apos;t start or stop the server: requests go to <span className="font-mono text-[12px]">{model.api_base || "its endpoints"}</span>. To run a model on your own cluster, <Link href="/deployments/new" className="underline underline-offset-2">launch it from Deployments</Link>.
          </p>
        )}
      </div>
    </section>
  );
}
