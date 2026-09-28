"use client";

import { useState, useTransition } from "react";
import { compactUsageAction, resyncCacheAction, testEnergyQueryAction } from "@/app/actions";
import { SettingsCard } from "@/components/access/ui";
import { BackupRestore } from "@/components/backup-restore";
import { Glyph } from "@/components/deployments/ui";
import { SelectField, Setting, Switch, TextField } from "@/components/models/fields";
import { Notice } from "@/components/models/ui";
import { Pill } from "@/components/overview/ui";
import { Button } from "@/components/ui/button";
import { useConfirm } from "@/components/ui/confirm-dialog";
import type { CharoSettingsView, CompressorStatusView, EnergySettingsView, ModelRoute, SlurmSettingsView, UsageRetentionView } from "@/lib/obleth";
import { describeResync } from "@/lib/settings-model";
import { cn } from "@/lib/utils";

/** Reads power from Prometheus to put watts, cost and CO₂ on Reports and the Overview. */
export function EnergySection({ settings }: { settings: EnergySettingsView }) {
  const [on, setOn] = useState(settings.enabled);
  const [url, setUrl] = useState(settings.prometheus_url);
  const [query, setQuery] = useState(settings.power_query);
  const [testing, start] = useTransition();
  const [result, setResult] = useState<{ ok: boolean; text: string } | null>(null);
  function test() {
    setResult(null);
    start(async () => {
      const res = await testEnergyQueryAction(url.trim(), query.trim());
      if (!res.ok) return setResult({ ok: false, text: res.error });
      setResult(res.data && res.data.node_count ? { ok: true, text: `${(res.data.cluster_watts / 1000).toFixed(1)} kW across ${res.data.node_count} node${res.data.node_count === 1 ? "" : "s"} right now.` } : { ok: false, text: "The query ran but returned no data." });
    });
  }
  return (
    <SettingsCard id="energy" title="Energy" description="Reads power from Prometheus to put watts, cost and CO₂ on Reports and the Overview." action={<Pill><Glyph glyph={settings.enabled ? "on" : "off"} className="h-[7px] w-[7px]" />{settings.enabled ? "On" : "Off"}</Pill>}>
      <Setting id="set-energy" label="Energy" hint="Off, no energy numbers are shown anywhere." fields={["energy.enabled"]} was={{ field: "energy.enabled", checkbox: true }}>
        <Switch name="energy.enabled" label="Energy on" checked={on} onChange={setOn} />
      </Setting>
      <Setting label="Prometheus" hint="Its URL and a PromQL query that returns watts per node." fields={["energy.prometheus_url", "energy.power_query"]}>
        <TextField name="energy.prometheus_url" label="Prometheus URL" type="url" value={url} onChange={(e) => setUrl(e.target.value)} placeholder="http://prometheus.monitoring:9090" mono />
        <textarea
          name="energy.power_query"
          aria-label="Power query"
          rows={2}
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder="sum by (instance) (node_power_watts)"
          spellCheck={false}
          className="w-full rounded-md border border-input bg-background px-3 py-2 font-mono text-[12.5px] focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-ring"
        />
        <div className="flex flex-wrap items-center gap-3">
          <Button type="button" variant="outline" size="sm" disabled={testing || !url.trim() || !query.trim()} onClick={test}>{testing ? "Testing…" : "Test the query"}</Button>
          <span className="text-xs text-muted-foreground">Uses what's typed here, before you save.</span>
        </div>
        {result && <p role="status" className={cn("text-[12.5px]", !result.ok && "font-medium")}>{result.text}</p>}
      </Setting>
      <Setting label="Costs" hint="How often power is read, what a kWh costs, grams of CO₂ per kWh, and the data centre's PUE." fields={["energy.poll_interval_secs", "energy.energy_cost_per_kwh", "energy.carbon_g_per_kwh", "energy.pue"]}>
        <div className="flex flex-wrap items-center gap-3">
          <span className="inline-flex items-center gap-1.5"><TextField name="energy.poll_interval_secs" label="Poll interval" inputMode="numeric" defaultValue={settings.poll_interval_secs} mono className="w-20" /><span className="text-xs text-muted-foreground">s</span></span>
          <span className="inline-flex items-center gap-1.5"><span className="text-xs text-muted-foreground">$</span><TextField name="energy.energy_cost_per_kwh" label="Cost per kWh" inputMode="decimal" defaultValue={settings.energy_cost_per_kwh} mono className="w-24" /><span className="text-xs text-muted-foreground">a kWh</span></span>
          <span className="inline-flex items-center gap-1.5"><TextField name="energy.carbon_g_per_kwh" label="Carbon per kWh" inputMode="decimal" defaultValue={settings.carbon_g_per_kwh} mono className="w-20" /><span className="text-xs text-muted-foreground">g CO₂ a kWh</span></span>
          <span className="inline-flex items-center gap-1.5"><span className="text-xs text-muted-foreground">PUE</span><TextField name="energy.pue" label="PUE" inputMode="decimal" defaultValue={settings.pue} mono className="w-20" /></span>
        </div>
      </Setting>
    </SettingsCard>
  );
}

/** The dashboard's assistant in the Playground. */
export function AssistantSection({ settings, models }: { settings: CharoSettingsView; models: ModelRoute[] }) {
  const [brain, setBrain] = useState(settings.brain_model ?? "");
  const brains = [...new Set([...(settings.brain_model ? [settings.brain_model] : []), ...models.filter((m) => m.enabled && m.supports_function_calling).map((m) => m.model_name)])].sort();
  return (
    <SettingsCard id="assistant" title="Assistant" description="The helper in the Playground that can test models, explain settings, and run benchmarks." action={<Pill><Glyph glyph={settings.enabled ? "on" : "off"} className="h-[7px] w-[7px]" />{settings.enabled ? "On" : "Off"}</Pill>}>
      <Setting id="set-assistant" label="Playground" hint="Off hides the Playground and its assistant from the dashboard." fields={["assistant.enabled"]} was={{ field: "assistant.enabled", checkbox: true }}>
        <Switch name="assistant.enabled" label="Playground on" defaultChecked={settings.enabled} />
      </Setting>
      <Setting label="Its model" hint="A model with function calling that thinks for the assistant. None makes it a plain model tester." fields={["assistant.brain_model"]} was={{ field: "assistant.brain_model" }}>
        <div className="w-72"><SelectField name="assistant.brain_model" label="Assistant model" value={brain} onChange={setBrain} options={[{ value: "", label: "None: a plain model tester" }, ...brains.map((n) => ({ value: n, label: n }))]} /></div>
      </Setting>
      <Setting label="Benchmarks" hint="Whether it may run load tests, and their limits." fields={["assistant.tool_run_benchmark", "assistant.bench_max_concurrency", "assistant.bench_max_duration_s", "assistant.bench_max_requests"]}>
        <Switch name="assistant.tool_run_benchmark" label="May run benchmarks" defaultChecked={settings.tools_enabled?.run_benchmark ?? false}>May run benchmarks</Switch>
        <div className="flex flex-wrap items-center gap-3">
          <span className="inline-flex items-center gap-1.5"><TextField name="assistant.bench_max_concurrency" label="Most at once" inputMode="numeric" defaultValue={settings.bench_max_concurrency} mono className="w-20" /><span className="text-xs text-muted-foreground">at once</span></span>
          <span className="inline-flex items-center gap-1.5"><TextField name="assistant.bench_max_duration_s" label="Longest run" inputMode="numeric" defaultValue={settings.bench_max_duration_s} mono className="w-20" /><span className="text-xs text-muted-foreground">s</span></span>
          <span className="inline-flex items-center gap-1.5"><TextField name="assistant.bench_max_requests" label="Most requests" inputMode="numeric" defaultValue={settings.bench_max_requests} mono className="w-20" /><span className="text-xs text-muted-foreground">requests</span></span>
        </div>
      </Setting>
    </SettingsCard>
  );
}

const PRESETS = [7, 30, 90, 180, 365];

/** How long request history is kept, a copy of every setting, and the data-plane cache. */
export function DataSection({ retention }: { retention: UsageRetentionView | null }) {
  const { confirm, confirmElement } = useConfirm();
  const [days, setDays] = useState(retention?.days ?? 180);
  const [pending, start] = useTransition();
  const [notice, setNotice] = useState<{ text: string; strong?: boolean } | null>(null);
  const choices = [...new Set([...PRESETS, retention?.days ?? 180])].sort((a, b) => a - b);

  function compact() {
    start(async () => {
      const ok = await confirm({ title: `Drop history older than ${retention?.days ?? days} days?`, description: "Request logs, reports and traces for those days are deleted now. This cannot be undone.", confirmLabel: "Drop it" });
      if (!ok) return;
      const res = await compactUsageAction();
      setNotice(res.ok ? { text: `Dropped ${res.partitionsDropped ?? 0} day${res.partitionsDropped === 1 ? "" : "s"} of history.` } : { text: res.error, strong: true });
    });
  }

  function reconcile() {
    start(async () => {
      const ok = await confirm({ title: "Reconcile the data-plane cache?", description: "Every key, model and MCP server is republished to the gateways, and entries with nothing behind them are removed. Requests keep flowing while it runs.", confirmLabel: "Reconcile" });
      if (!ok) return;
      const res = await resyncCacheAction();
      setNotice(res.ok && res.report ? { text: describeResync(res.report) } : { text: res.ok ? "It ran, but returned no report." : res.error, strong: true });
    });
  }

  return (
    <SettingsCard id="data" title="Data and backups" description="How long request history is kept, and a copy of every setting.">
      {confirmElement}
      {notice && <div className="px-[18px] pb-3"><Notice strong={notice.strong} onDismiss={() => setNotice(null)}>{notice.text}</Notice></div>}
      {retention ? (
        <Setting id="set-retention" label="Keep request history for" hint="Request logs, reports and traces older than this are dropped by day." fields={["retention.days"]} was={{ field: "retention.days" }}>
          <input type="hidden" name="retention.days" value={String(days)} />
          <div className="flex flex-wrap gap-1.5">
            {choices.map((d) => (
              <button key={d} type="button" aria-pressed={days === d} onClick={() => setDays(d)} className={cn("inline-flex h-8 items-center rounded-full border px-3 text-[12.5px]", days === d ? "border-foreground bg-secondary text-foreground" : "border-border text-muted-foreground hover:text-foreground")}>
                {d} days{d === retention.days ? " · now" : ""}
              </button>
            ))}
          </div>
          {days < retention.days && <span className="text-xs font-medium">Saving asks first: history older than {days} days is dropped.</span>}
        </Setting>
      ) : (
        <p className="px-[18px] py-3 text-[12.5px] text-muted-foreground">The retention setting couldn&apos;t be read.</p>
      )}
      <Setting label="Drop old history now" hint="Drops the days past the window straight away. Asks first.">
        <Button type="button" variant="outline" size="sm" className="self-start" disabled={pending || !retention} onClick={compact}>Drop what&apos;s past {retention?.days ?? days} days…</Button>
      </Setting>
      <Setting label="Backup" hint="Your models, tenants, keys, MCP servers and settings as one file. Provider secrets stay encrypted, so restoring needs the same OBLETH_ENCRYPTION_KEY; alert credentials are included as stored.">
        <BackupRestore />
      </Setting>
      <Setting label="Data-plane cache" hint="Republishes every key, model and MCP server to the gateways and removes what's stale. For after a restore, or when a delete says the cache couldn't be updated.">
        <Button type="button" variant="outline" size="sm" className="self-start" disabled={pending} onClick={reconcile}>Reconcile cache…</Button>
      </Setting>
    </SettingsCard>
  );
}

export interface VersionInfo {
  gateway: { version: string; git_sha: string | null; built_at: string | null } | null;
  controlPlane: { version: string; sha: string | null };
  latest: string | null;
  updateAvailable: boolean;
}

function Row({ label, children, mark }: { label: string; children: React.ReactNode; mark: React.ReactNode }) {
  return (
    <div className="grid grid-cols-[150px_minmax(0,1fr)_auto] items-center gap-3 border-t border-border px-[18px] py-2.5 text-[13px] first:border-t-0">
      <span className="text-muted-foreground">{label}</span>
      <span className="min-w-0">{children}</span>
      {mark}
    </div>
  );
}

/** What's running: the gateway, the control plane, the provisioner and the compressor. */
export function AboutSection({ version, slurm, compressor }: { version: VersionInfo; slurm: SlurmSettingsView | null; compressor: CompressorStatusView | null }) {
  const g = version.gateway;
  const mismatch = g && g.version !== version.controlPlane.version;
  const sha = (s: string | null | undefined) => (s ? ` · ${s.slice(0, 7)}` : "");
  return (
    <SettingsCard id="about" title="About">
      <Row label="Gateway" mark={version.updateAvailable ? <Pill inverted>{version.latest} is out</Pill> : <Pill><Glyph glyph="on" className="h-[7px] w-[7px]" />{version.latest ? "Latest" : "Running"}</Pill>}>
        {g ? <><span className="font-mono text-[12px]">{g.version}{sha(g.git_sha)}</span>{g.built_at && <span className="text-muted-foreground"> · built {new Date(g.built_at).toLocaleDateString([], { month: "short", day: "numeric" })}</span>}</> : <span className="text-muted-foreground">Couldn&apos;t be reached</span>}
      </Row>
      <Row label="Control plane" mark={mismatch ? <Pill inverted>Differs from the gateway</Pill> : <Pill><Glyph glyph="on" className="h-[7px] w-[7px]" />Matches</Pill>}>
        <span className="font-mono text-[12px]">{version.controlPlane.version}{sha(version.controlPlane.sha)}</span>
      </Row>
      <Row label="Provisioner" mark={slurm?.provisioner_running ? <Pill><Glyph glyph={slurm.provisioner_tick_status === "error" ? "half" : "on"} className="h-[7px] w-[7px]" />Running</Pill> : <Pill><Glyph glyph="off" className="h-[7px] w-[7px]" />Not seen</Pill>}>
        {slurm?.provisioner_running ? (
          <><span className="font-mono text-[12px]">{slurm.provisioner_version ?? "?"}{sha(slurm.provisioner_git_sha)}</span><span className="text-muted-foreground"> · checked in {slurm.provisioner_last_seen_secs ?? "?"} s ago{slurm.provisioner_tick_detail ? ` · ${slurm.provisioner_tick_detail}` : ""}</span></>
        ) : (
          <span className="text-muted-foreground">Only needed to launch models on Slurm.</span>
        )}
      </Row>
      <Row label="Compressor" mark={compressor?.configured ? (compressor.reachable ? <Pill><Glyph glyph="on" className="h-[7px] w-[7px]" />Answering</Pill> : <Pill inverted>Not answering</Pill>) : <Pill><Glyph glyph="off" className="h-[7px] w-[7px]" />None</Pill>}>
        {compressor?.configured ? (
          <span className={cn(!compressor.reachable && "font-medium")}>{compressor.reachable ? <><span className="font-mono text-[12px]">{compressor.model ?? "model"}{compressor.revision ? ` · ${compressor.revision.slice(0, 7)}` : ""}</span><span className="text-muted-foreground"> · {compressor.url}</span></> : compressor.error ?? `${compressor.url} doesn't answer`}</span>
        ) : (
          <span className="text-muted-foreground">Not set up. Lossy compression needs it (OBLETH_COMPRESSOR_URL).</span>
        )}
      </Row>
    </SettingsCard>
  );
}
