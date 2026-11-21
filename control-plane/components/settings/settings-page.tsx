"use client";

import { useCallback, useState } from "react";
import { useRouter } from "next/navigation";
import { saveSettingsAction } from "@/app/actions";
import { AlertsSection } from "@/components/settings/alerts-section";
import { BoonsSection } from "@/components/settings/boons-section";
import { AboutSection, AssistantSection, DataSection, EnergySection, KeysSection, type VersionInfo } from "@/components/settings/other-sections";
import { NeedsYouSection, needsYouCount } from "@/components/settings/needs-you";
import { RoutingSection } from "@/components/settings/routing-section";
import { NetworksSection } from "@/components/settings/networks-section";
import { useConfirm } from "@/components/ui/confirm-dialog";
import type {
  AdviceReportView,
  AlertSettingsView,
  AutoRouterSettingsView,
  BoonSettingsView,
  CharoSettingsView,
  CompressorStatusView,
  EnergySettingsView,
  KeyPrefixView,
  NetworkSettingsView,
  KnowledgeSettingsView,
  ModelRoute,
  RouterReadinessView,
  SlurmSettingsView,
  UsageRetentionView,
} from "@/lib/obleth";
import { BOONS, needsYou, sectionOfField } from "@/lib/settings-model";
import { SettingsCard } from "@/components/ui/settings-card";
import { SettingsForm } from "@/components/ui/settings-form";
import { jumpTo, SettingsNav, type FindItem, type NavItem } from "@/components/ui/settings-nav";

export interface SettingsData {
  /** What the last day of requests says to change; null when it could not be read. */
  advice: AdviceReportView | null;
  alerts: AlertSettingsView | null;
  router: AutoRouterSettingsView | null;
  readiness: RouterReadinessView | null;
  boons: BoonSettingsView | null;
  compressor: CompressorStatusView | null;
  knowledge: KnowledgeSettingsView | null;
  energy: EnergySettingsView | null;
  charo: CharoSettingsView | null;
  retention: UsageRetentionView | null;
  /** The prefix new API keys start with; null when it could not be read. */
  keyPrefix: KeyPrefixView | null;
  networks: NetworkSettingsView | null;
  slurm: SlurmSettingsView | null;
  models: ModelRoute[];
  version: VersionInfo;
}

const SEARCH: FindItem[] = [
  { id: "set-slack", label: "Slack alerts", words: "webhook alert notify" },
  { id: "set-email", label: "Email alerts", words: "smtp mail alert notify recipients" },
  { id: "set-quiet", label: "Quiet period", words: "cooldown interval repeat alert" },
  { id: "set-router-profile", label: "Routing profile", words: "auto router balanced cost best" },
  { id: "set-router-weights", label: "Routing weights", words: "capacity price cost tag task fit" },
  { id: "set-classifier", label: "Router classifier", words: "intent tags classify auto" },
  { id: "set-difficulty", label: "Hard questions", words: "difficulty tier stronger" },
  { id: "set-sticky", label: "Conversations", words: "sticky conversation escalate stay model" },
  { id: "set-anthropic", label: "Anthropic clients", words: "messages claude default model" },
  { id: "boon-vision", label: "Vision", words: "image describe boon" },
  { id: "boon-structured_output", label: "Structured output", words: "json schema fixer boon" },
  { id: "boon-tool_loop", label: "Tool loop", words: "mcp tools turns boon" },
  { id: "boon-image_generation", label: "Image generation", words: "draw picture flux boon" },
  { id: "boon-web_search", label: "Web search", words: "search web internet searxng lookup boon" },
  { id: "boon-speculation", label: "Speculation", words: "drafter draft verify gates boon" },
  { id: "boon-compression", label: "Compression", words: "dedup lossy compact boon" },
  { id: "set-energy", label: "Energy", words: "power prometheus watts carbon co2 pue kwh" },
  { id: "set-assistant", label: "Assistant", words: "charo playground agent benchmark" },
  { id: "set-key-prefix", label: "Key prefix", words: "api key prefix obl sk format" },
  { id: "set-named-networks", label: "Named networks", words: "network name cidr range campus vpn cluster private address subnet ours own" },
  { id: "set-client-address", label: "Where a request came from", words: "client ip address trusted proxies x-forwarded-for cf-connecting-ip country header" },
  { id: "set-retention", label: "History retention", words: "retention days usage logs keep" },
  { id: "data", label: "Backup and restore", words: "backup export restore cache reconcile" },
  { id: "about", label: "Versions", words: "version about provisioner compressor" },
];

function Unreadable({ id, title }: { id: string; title: string }) {
  return (
    <SettingsCard id={id} title={title}>
      <p className="px-[18px] py-3.5 text-[12.5px] text-secondary-foreground">These settings couldn&apos;t be read from the gateway, so they can&apos;t be changed here right now. Nothing was overwritten. Reload to try again.</p>
    </SettingsCard>
  );
}

/** Settings that live on other pages, linked below the sections. */
const ELSEWHERE: NavItem[] = [
  { id: "slurm", label: "Slurm ›", count: "Deployments", href: "/deployments?slurm=1" },
  { id: "retrieval", label: "Retrieval ›", count: "Knowledge", href: "/knowledge?tab=retrieval" },
];

export function SettingsPage({ data, anchor }: { data: SettingsData; anchor?: string }) {
  const router = useRouter();
  const { confirm, confirmElement } = useConfirm();
  const [dirty, setDirty] = useState<string[]>([]);
  const findings = needsYou({ alerts: data.alerts, boons: data.boons, knowledge: data.knowledge, models: data.models, readiness: data.readiness, slurm: data.slurm, compressor: data.compressor, router: data.router });
  const needsCount = needsYouCount(findings, data.advice);
  const boonsOn = data.boons ? BOONS.filter((b) => data.boons![b.enabled]).length : null;

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

  const nav: NavItem[] = [
    { id: "needs", label: "Needs you", count: needsCount || undefined },
    { id: "alerts", label: "Alerts", count: data.alerts ? (data.alerts.slack_webhook_set || data.alerts.email ? "on" : "none") : undefined },
    { id: "routing", label: "Routing", count: data.router ? (data.router.classifier_enabled ? "auto on" : "heuristics") : undefined },
    { id: "boons", label: "Boons", count: boonsOn !== null ? `${boonsOn} of ${BOONS.length} on` : undefined },
    { id: "energy", label: "Energy", count: data.energy ? (data.energy.enabled ? "on" : "off") : undefined },
    { id: "assistant", label: "Assistant", count: data.charo ? (data.charo.enabled ? "on" : "off") : undefined },
    { id: "keys", label: "Keys", count: data.keyPrefix ? `${data.keyPrefix.prefix}_` : undefined },
    { id: "networks", label: "Networks", count: data.networks ? (data.networks.networks.length ? `${data.networks.networks.length} named` : "none named") : undefined },
    { id: "data", label: "Data and backups", count: data.retention ? `${data.retention.days} days` : undefined },
    { id: "about", label: "About", count: data.version.gateway?.version },
  ];
  const sectionOf = useCallback((n: string) => sectionOfField(n), []);
  const labelOf = useCallback((n: string) => {
    const [section, field = ""] = n.split(".");
    const words = field.replace(/^(speculation|compression|image_generation|web_search|structured_output|tool_loop|vision)_/, "$1 ").replace(/_/g, " ");
    return `${section === "retention" ? "Data" : section[0].toUpperCase() + section.slice(1)} · ${words}`;
  }, []);
  const onDirty = useCallback((sections: string[]) => setDirty(sections.map((s) => (s === "retention" ? "data" : s))), []);

  return (
    <div className="mx-auto flex max-w-[1500px] flex-col gap-5">
      {confirmElement}
      <div className="flex flex-col gap-1.5">
        <h1 className="text-[26px] font-semibold tracking-tight">Settings</h1>
        <p className="text-[13px] text-secondary-foreground">How the whole gateway behaves. Changes apply at once, no restart.{needsCount ? ` ${needsCount} thing${needsCount === 1 ? " needs" : "s need"} you.` : ""}</p>
      </div>

      <div className="grid items-start gap-6 lg:grid-cols-[200px_minmax(0,1fr)]">
        <SettingsNav label="Settings sections" items={nav} after={ELSEWHERE} find={SEARCH} findId="settings-search-results" dirty={dirty} anchor={anchor} />

        <div className="flex min-w-0 flex-col gap-4">
          <NeedsYouSection findings={findings} advice={data.advice} jump={jumpTo} />

          <SettingsForm id="settings" sectionOf={sectionOf} labelOf={labelOf} save={saveSettingsAction} confirmSave={confirmSave} onSaved={() => router.refresh()} onDirtyChange={onDirty} className="flex flex-col gap-4" ariaLabel="Gateway settings">
            {data.alerts ? <AlertsSection settings={data.alerts} /> : <Unreadable id="alerts" title="Alerts" />}
            {data.router ? <RoutingSection settings={data.router} models={data.models} readiness={data.readiness} /> : <Unreadable id="routing" title="Routing" />}
            {data.boons ? <BoonsSection settings={data.boons} models={data.models} knowledge={data.knowledge} compressor={data.compressor} /> : <Unreadable id="boons" title="Boons" />}
            {data.energy ? <EnergySection settings={data.energy} /> : <Unreadable id="energy" title="Energy" />}
            {data.charo ? <AssistantSection settings={data.charo} models={data.models} /> : <Unreadable id="assistant" title="Assistant" />}
            {data.keyPrefix ? <KeysSection settings={data.keyPrefix} /> : <Unreadable id="keys" title="Keys" />}
            {data.networks ? <NetworksSection settings={data.networks} /> : <Unreadable id="networks" title="Networks" />}
            <DataSection retention={data.retention} />
          </SettingsForm>

          <AboutSection version={data.version} slurm={data.slurm} compressor={data.compressor} />
        </div>
      </div>
    </div>
  );
}
