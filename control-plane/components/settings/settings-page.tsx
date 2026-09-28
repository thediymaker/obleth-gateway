"use client";

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { Search } from "lucide-react";
import { saveSettingsAction } from "@/app/actions";
import { SettingsForm } from "@/components/access/settings-form";
import { SettingsCard } from "@/components/access/ui";
import { AlertsSection } from "@/components/settings/alerts-section";
import { BoonsSection } from "@/components/settings/boons-section";
import { AboutSection, AssistantSection, DataSection, EnergySection, type VersionInfo } from "@/components/settings/other-sections";
import { RoutingSection } from "@/components/settings/routing-section";
import { useConfirm } from "@/components/ui/confirm-dialog";
import type {
  AlertSettingsView,
  AutoRouterSettingsView,
  BoonSettingsView,
  CharoSettingsView,
  CompressorStatusView,
  EnergySettingsView,
  KnowledgeSettingsView,
  ModelRoute,
  RouterReadinessView,
  SlurmSettingsView,
  UsageRetentionView,
} from "@/lib/obleth";
import { BOONS, needsYou, sectionOfField } from "@/lib/settings-model";
import { cn } from "@/lib/utils";

export interface SettingsData {
  alerts: AlertSettingsView | null;
  router: AutoRouterSettingsView | null;
  readiness: RouterReadinessView | null;
  boons: BoonSettingsView | null;
  compressor: CompressorStatusView | null;
  knowledge: KnowledgeSettingsView | null;
  energy: EnergySettingsView | null;
  charo: CharoSettingsView | null;
  retention: UsageRetentionView | null;
  slurm: SlurmSettingsView | null;
  models: ModelRoute[];
  version: VersionInfo;
}

const SEARCH: { id: string; label: string; words: string }[] = [
  { id: "set-slack", label: "Slack alerts", words: "webhook alert notify" },
  { id: "set-email", label: "Email alerts", words: "smtp mail alert notify recipients" },
  { id: "set-quiet", label: "Quiet period", words: "cooldown interval repeat alert" },
  { id: "set-router-profile", label: "Routing profile", words: "auto router balanced cost best" },
  { id: "set-router-weights", label: "Routing weights", words: "capacity price cost tag task fit" },
  { id: "set-classifier", label: "Router classifier", words: "intent tags classify auto" },
  { id: "set-difficulty", label: "Hard questions", words: "difficulty tier stronger" },
  { id: "set-anthropic", label: "Anthropic clients", words: "messages claude default model" },
  { id: "boon-vision", label: "Vision", words: "image describe boon" },
  { id: "boon-structured_output", label: "Structured output", words: "json schema fixer boon" },
  { id: "boon-tool_loop", label: "Tool loop", words: "mcp tools turns boon" },
  { id: "boon-image_generation", label: "Image generation", words: "draw picture flux boon" },
  { id: "boon-speculation", label: "Speculation", words: "drafter draft verify gates boon" },
  { id: "boon-compression", label: "Compression", words: "dedup lossy compact boon" },
  { id: "set-energy", label: "Energy", words: "power prometheus watts carbon co2 pue kwh" },
  { id: "set-assistant", label: "Assistant", words: "charo playground agent benchmark" },
  { id: "set-retention", label: "History retention", words: "retention days usage logs keep" },
  { id: "data", label: "Backup and restore", words: "backup export restore cache reconcile" },
  { id: "about", label: "Versions", words: "version about provisioner compressor" },
];

function jump(id: string) {
  const el = document.getElementById(id);
  if (!el) return;
  el.scrollIntoView({ behavior: "smooth", block: id.startsWith("set-") || id.startsWith("boon-") ? "center" : "start" });
  if (id.startsWith("set-") || id.startsWith("boon-")) {
    el.classList.remove("setting-flash");
    void el.offsetWidth;
    el.classList.add("setting-flash");
    setTimeout(() => el.classList.remove("setting-flash"), 1800);
  }
}

function FindSetting() {
  const [query, setQuery] = useState("");
  const [open, setOpen] = useState(false);
  const [index, setIndex] = useState(0);
  const input = useRef<HTMLInputElement>(null);
  const results = useMemo(() => {
    const q = query.trim().toLowerCase();
    return q ? SEARCH.filter((s) => `${s.label} ${s.words}`.toLowerCase().includes(q)).slice(0, 6) : [];
  }, [query]);
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") { e.preventDefault(); input.current?.focus(); input.current?.select(); }
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, []);
  const choose = (id: string) => { setOpen(false); setQuery(""); input.current?.blur(); jump(id); };
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
            if (e.key === "Enter" && results[index]) { e.preventDefault(); choose(results[index].id); }
            if (e.key === "Escape") { setQuery(""); input.current?.blur(); }
          }}
          placeholder="Find a setting"
          aria-label="Find a setting"
          role="combobox"
          aria-expanded={open && results.length > 0}
          aria-controls="settings-search-results"
          className="min-w-0 flex-1 bg-transparent outline-none placeholder:text-muted-foreground"
        />
        <kbd className="rounded border border-border px-1 font-mono text-[10.5px] text-muted-foreground">⌘K</kbd>
      </label>
      {open && query.trim() !== "" && (
        <div id="settings-search-results" role="listbox" aria-label="Matching settings" className="absolute left-0 top-10 z-40 w-[min(300px,calc(100vw-2rem))] rounded-[10px] border border-border bg-[hsl(240_5%_9%)] p-1.5 shadow-2xl">
          {results.length === 0 ? <p className="px-2.5 py-2 text-xs text-muted-foreground">No setting matches “{query}”.</p> : results.map((r, i) => (
            <button key={r.id} type="button" role="option" aria-selected={i === index} onMouseDown={(e) => e.preventDefault()} onClick={() => choose(r.id)} onMouseEnter={() => setIndex(i)} className={cn("flex w-full rounded-md px-2.5 py-2 text-left text-[13px]", i === index && "bg-secondary")}>{r.label}</button>
          ))}
        </div>
      )}
    </div>
  );
}

function Unreadable({ id, title }: { id: string; title: string }) {
  return (
    <SettingsCard id={id} title={title}>
      <p className="px-[18px] py-3.5 text-[12.5px] text-secondary-foreground">These settings couldn&apos;t be read from the gateway, so they can&apos;t be changed here right now. Nothing was overwritten. Reload to try again.</p>
    </SettingsCard>
  );
}

export function SettingsPage({ data, anchor }: { data: SettingsData; anchor?: string }) {
  const router = useRouter();
  const { confirm, confirmElement } = useConfirm();
  const [active, setActive] = useState("needs");
  const [dirty, setDirty] = useState<string[]>([]);
  const findings = needsYou({ alerts: data.alerts, boons: data.boons, knowledge: data.knowledge, models: data.models, readiness: data.readiness, slurm: data.slurm, compressor: data.compressor, router: data.router });
  const boonsOn = data.boons ? BOONS.filter((b) => data.boons![b.enabled]).length : null;

  useEffect(() => {
    const id = anchor || window.location.hash.slice(1);
    if (id) requestAnimationFrame(() => jump(id));
  }, [anchor]);

  useEffect(() => {
    const ids = ["needs", "alerts", "routing", "boons", "energy", "assistant", "data", "about"];
    const els = ids.map((id) => document.getElementById(id)).filter((e): e is HTMLElement => !!e);
    const observer = new IntersectionObserver((entries) => {
      const visible = entries.filter((e) => e.isIntersecting).sort((a, b) => a.boundingClientRect.top - b.boundingClientRect.top);
      if (visible[0]) setActive(visible[0].target.id);
    }, { rootMargin: "-20% 0px -65% 0px" });
    els.forEach((e) => observer.observe(e));
    return () => observer.disconnect();
  }, []);

  const confirmSave = useCallback(async (form: FormData, sections: string[]) => {
    if (sections.includes("retention") && data.retention) {
      const days = Number(form.get("retention.days"));
      if (days < data.retention.days && !(await confirm({ title: `Keep history for ${days} days?`, description: `Request logs, reports and traces older than ${days} days are dropped. This cannot be undone.`, confirmLabel: "Keep less" }))) return false;
    }
    if (sections.includes("alerts") && data.alerts?.email && form.get("alerts.email_enabled") !== "on") {
      if (!(await confirm({ title: "Turn off email alerts?", description: "Its server, sign-in and recipients are deleted, so turning it on again means entering them again.", confirmLabel: "Turn off" }))) return false;
    }
    if (sections.includes("alerts") && form.get("alerts.clear_slack") === "on" && !String(form.get("alerts.slack_webhook_url") ?? "").trim()) {
      if (!(await confirm({ title: "Remove the Slack webhook?", description: "Alerts stop going to Slack.", confirmLabel: "Remove" }))) return false;
    }
    return true;
  }, [confirm, data.alerts, data.retention]);

  const nav: { id: string; label: string; count?: string | number; href?: string }[] = [
    { id: "needs", label: "Needs you", count: findings.length || undefined },
    { id: "alerts", label: "Alerts", count: data.alerts ? (data.alerts.slack_webhook_set || data.alerts.email ? "on" : "none") : undefined },
    { id: "routing", label: "Routing", count: data.router ? (data.router.classifier_enabled ? "auto on" : "heuristics") : undefined },
    { id: "boons", label: "Boons", count: boonsOn !== null ? `${boonsOn} of ${BOONS.length} on` : undefined },
    { id: "energy", label: "Energy", count: data.energy ? (data.energy.enabled ? "on" : "off") : undefined },
    { id: "assistant", label: "Assistant", count: data.charo ? (data.charo.enabled ? "on" : "off") : undefined },
    { id: "data", label: "Data and backups", count: data.retention ? `${data.retention.days} days` : undefined },
    { id: "about", label: "About", count: data.version.gateway?.version },
  ];
  const sectionOf = useCallback((n: string) => sectionOfField(n), []);
  const labelOf = useCallback((n: string) => {
    const [section, field = ""] = n.split(".");
    const words = field.replace(/^(speculation|compression|image_generation|structured_output|tool_loop|vision)_/, "$1 ").replace(/_/g, " ");
    return `${section === "retention" ? "Data" : section[0].toUpperCase() + section.slice(1)} · ${words}`;
  }, []);
  const onDirty = useCallback((sections: string[]) => setDirty(sections.map((s) => (s === "retention" ? "data" : s))), []);

  return (
    <div className="mx-auto flex max-w-[1500px] flex-col gap-5">
      {confirmElement}
      <div className="flex flex-col gap-1.5">
        <h1 className="text-[26px] font-semibold tracking-tight">Settings</h1>
        <p className="text-[13px] text-secondary-foreground">How the whole gateway behaves. Changes apply at once, no restart.{findings.length ? ` ${findings.length} thing${findings.length === 1 ? " needs" : "s need"} you.` : ""}</p>
      </div>

      <div className="grid items-start gap-6 lg:grid-cols-[200px_minmax(0,1fr)]">
        <nav aria-label="Settings sections" className="flex flex-col gap-0.5 lg:sticky lg:top-0">
          <div className="mb-2"><FindSetting /></div>
          {nav.map((n) => (
            <a key={n.id} href={`#${n.id}`} onClick={(e) => { e.preventDefault(); jump(n.id); history.replaceState(null, "", `#${n.id}`); }} aria-current={active === n.id ? "true" : undefined} className={cn("flex h-8 items-center justify-between rounded-lg px-2.5 text-[13px] transition-colors", active === n.id ? "bg-secondary text-foreground" : "text-muted-foreground hover:bg-muted/40 hover:text-foreground")}>
              {n.label}
              <span className="flex items-center gap-1.5 text-[11.5px] text-muted-foreground">{n.count}{dirty.includes(n.id) && <span aria-label="unsaved changes" className="h-1.5 w-1.5 rounded-full bg-foreground" />}</span>
            </a>
          ))}
          <div className="mx-1 my-2 h-px bg-border" />
          <Link href="/deployments?slurm=1" className="flex h-8 items-center justify-between rounded-lg px-2.5 text-[13px] text-muted-foreground hover:bg-muted/40 hover:text-foreground">Slurm ›<span className="text-[11.5px]">Deployments</span></Link>
          <Link href="/knowledge?tab=retrieval" className="flex h-8 items-center justify-between rounded-lg px-2.5 text-[13px] text-muted-foreground hover:bg-muted/40 hover:text-foreground">Retrieval ›<span className="text-[11.5px]">Knowledge</span></Link>
        </nav>

        <div className="flex min-w-0 flex-col gap-4">
          <section id="needs" aria-label="Needs you" className={cn("scroll-mt-24 rounded-xl bg-card", findings.length ? "border-[1.5px] border-foreground" : "border border-border")}>
            <h2 className="px-[18px] pb-2 pt-3.5 text-sm font-semibold">{findings.length ? `Needs you · ${findings.length}` : "Nothing needs you"}</h2>
            {findings.length === 0 && <p className="px-[18px] pb-3.5 text-[12.5px] text-muted-foreground">Alerts reach someone, every boon a model asks for is on, and routing has nothing to fix.</p>}
            {findings.map((f) => (
              <div key={f.key} className="grid grid-cols-[minmax(0,1fr)_auto] items-center gap-4 border-t border-border px-[18px] py-3">
                <div className="min-w-0"><p className="text-[13.5px] font-medium">{f.title}</p><p className="mt-0.5 text-[12.5px] text-secondary-foreground">{f.detail}</p></div>
                {f.href.startsWith("#") ? (
                  <a href={f.href} onClick={(e) => { e.preventDefault(); jump(f.href.slice(1)); }} className="inline-flex h-8 items-center rounded-lg border border-border px-3 text-[12.5px] hover:border-muted-foreground/60">{f.action}</a>
                ) : (
                  <Link href={f.href} className="inline-flex h-8 items-center rounded-lg border border-border px-3 text-[12.5px] hover:border-muted-foreground/60">{f.action}</Link>
                )}
              </div>
            ))}
          </section>

          <SettingsForm id="settings" sectionOf={sectionOf} labelOf={labelOf} save={saveSettingsAction} confirmSave={confirmSave} onSaved={() => router.refresh()} onDirtyChange={onDirty} className="flex flex-col gap-4" ariaLabel="Gateway settings">
            {data.alerts ? <AlertsSection settings={data.alerts} /> : <Unreadable id="alerts" title="Alerts" />}
            {data.router ? <RoutingSection settings={data.router} models={data.models} readiness={data.readiness} /> : <Unreadable id="routing" title="Routing" />}
            {data.boons ? <BoonsSection settings={data.boons} models={data.models} knowledge={data.knowledge} compressor={data.compressor} /> : <Unreadable id="boons" title="Boons" />}
            {data.energy ? <EnergySection settings={data.energy} /> : <Unreadable id="energy" title="Energy" />}
            {data.charo ? <AssistantSection settings={data.charo} models={data.models} /> : <Unreadable id="assistant" title="Assistant" />}
            <DataSection retention={data.retention} />
          </SettingsForm>

          <AboutSection version={data.version} slurm={data.slurm} compressor={data.compressor} />
        </div>
      </div>
    </div>
  );
}
