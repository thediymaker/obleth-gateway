"use client";

import { useCallback, useState, useTransition } from "react";
import { setSlurmSettingsAction, testSlurmConnectionAction } from "@/app/actions";
import { SettingsForm, type SaveResult } from "@/components/access/settings-form";
import { Glyph } from "@/components/deployments/ui";
import { Setting, Switch, TextField } from "@/components/models/fields";
import { Notice, Sheet } from "@/components/models/ui";
import { Button } from "@/components/ui/button";
import { useConfirm } from "@/components/ui/confirm-dialog";
import { duration } from "@/lib/deployments-model";
import type { NodeAlias, SlurmHealthView, SlurmSettingsView, UpdateSlurmSettings } from "@/lib/obleth";

const FIELDS = ["enabled", "url", "api_version", "user", "jwt", "aliases"];
const LABELS: Record<string, string> = { enabled: "Slurm", url: "slurmrestd URL", api_version: "API version", user: "Slurm user", jwt: "JWT", aliases: "Node addresses" };

function since(secs: number | null): string {
  if (secs == null) return "never";
  return secs < 5 ? "just now" : `${duration(secs * 1000)} ago`;
}

/** What the provisioner is doing, in one line, from what it last reported. */
export function provisionerLine(s: SlurmSettingsView): { glyph: "on" | "half" | "off"; text: string; detail?: string } {
  if (!s.provisioner_running) {
    return {
      glyph: "off",
      text: `Provisioner not running · last seen ${since(s.provisioner_last_seen_secs)}`,
      detail: "Saving here only stores the connection. The obleth-provisioner process launches jobs: in Kubernetes set provisioner.enabled=true; in Docker add slurm to COMPOSE_PROFILES.",
    };
  }
  const failing = s.provisioner_tick_status != null && s.provisioner_tick_status !== "ok" && s.provisioner_tick_status !== "idle" && (s.provisioner_held_secs ?? 0) > 60;
  if (failing) {
    return {
      glyph: "half",
      text: `Provisioner running, but its passes are failing · last good pass ${since(s.provisioner_last_ok_secs)}`,
      detail: `${s.provisioner_tick_detail ? `${s.provisioner_tick_detail}. ` : ""}Replica states on Deployments are frozen until this clears. Test the connection below.`,
    };
  }
  const version = s.provisioner_version ? ` · ${s.provisioner_version}${s.provisioner_git_sha ? ` ${s.provisioner_git_sha.slice(0, 7)}` : ""}` : "";
  return { glyph: "on", text: `Provisioner checked in ${since(s.provisioner_last_seen_secs)}${s.provisioner_tick_status === "idle" ? " · idle" : ""}${version}` };
}

/** How obleth reaches slurmrestd to launch models as jobs. Moved here from Settings. */
export function SlurmConnection({ settings, replicas, onClose, onSaved }: { settings: SlurmSettingsView; replicas: number; onClose: () => void; onSaved: () => void }) {
  const { confirm, confirmElement } = useConfirm();
  const [enabled, setEnabled] = useState(settings.enabled);
  const [aliases, setAliases] = useState<NodeAlias[]>(settings.node_aliases);
  const [testing, startTest] = useTransition();
  const [health, setHealth] = useState<SlurmHealthView | null>(null);
  const [testError, setTestError] = useState<string | null>(null);
  const line = provisionerLine(settings);

  const save = useCallback(async (data: FormData): Promise<SaveResult> => {
    const body: UpdateSlurmSettings = {
      enabled: data.get("enabled") === "on",
      slurmrestd_url: String(data.get("url") ?? "").trim(),
      slurmrestd_api_version: String(data.get("api_version") ?? "").trim() || "v0.0.40",
      slurm_user: String(data.get("user") ?? "").trim(),
      node_aliases: (JSON.parse(String(data.get("aliases") || "[]")) as NodeAlias[]).map((a) => ({ host: a.host.trim(), ip: a.ip.trim() })).filter((a) => a.host || a.ip),
    };
    const jwt = String(data.get("jwt") ?? "").trim();
    if (jwt) body.slurm_jwt = jwt;
    if (body.enabled && (!body.slurmrestd_url || !body.slurm_user)) return { ok: false, error: "Slurm needs a slurmrestd URL and a user to turn on.", saved: [] };
    const res = await setSlurmSettingsAction(body);
    if (!res.ok) return { ok: false, error: res.error, saved: [] };
    setHealth(null);
    onSaved();
    return { ok: true };
  }, [onSaved]);

  const confirmSave = useCallback(async (data: FormData) => {
    if (settings.enabled && data.get("enabled") !== "on") {
      return confirm({
        title: "Turn Slurm off?",
        description: `obleth stops launching and replacing jobs.${replicas ? ` The ${replicas} running now keep their nodes, and their state freezes; to release them, set those deployments to 0 replicas first.` : ""}`,
        confirmLabel: "Turn off",
      });
    }
    return true;
  }, [confirm, replicas, settings.enabled]);

  const sectionOf = useCallback((n: string) => (FIELDS.includes(n) ? "slurm" : null), []);
  const labelOf = useCallback((n: string) => LABELS[n] ?? n, []);

  function test() {
    setHealth(null);
    setTestError(null);
    startTest(async () => {
      const res = await testSlurmConnectionAction();
      if (res.ok) setHealth(res.health ?? null);
      else setTestError(res.error);
    });
  }

  const jwt = health?.jwt;
  return (
    <>
      {confirmElement}
      <Sheet open onClose={onClose} title="Slurm connection" width="w-[min(620px,100vw)]" description="How obleth reaches slurmrestd to launch models as jobs, keep them at a replica count, and route to them once healthy.">
        <div className="flex flex-col gap-3 px-6 pb-4">
          {settings.enabled && (
            <div className="flex flex-col gap-1 rounded-lg border border-border px-3 py-2.5 text-[12.5px]">
              <span className="flex items-center gap-2 font-medium"><Glyph glyph={line.glyph} className="h-[7px] w-[7px]" />{line.text}</span>
              {line.detail && <span className="text-secondary-foreground">{line.detail}</span>}
            </div>
          )}
          <div className="flex flex-wrap items-center gap-3">
            <Button type="button" variant="outline" size="sm" disabled={testing || !settings.slurmrestd_url} onClick={test}>{testing ? "Testing…" : "Test the connection"}</Button>
            <span className="text-xs text-muted-foreground">{settings.slurmrestd_url ? "Uses what's saved." : "Save a URL first."}</span>
          </div>
          {testError && <Notice strong onDismiss={() => setTestError(null)}>{testError}</Notice>}
          {health && (
            <div className="flex flex-col gap-1.5 rounded-lg border border-border px-3 py-2.5 text-[12.5px]">
              <span className="flex items-center gap-2">
                {health.ping.ok ? <Glyph glyph="on" className="h-[7px] w-[7px]" /> : <span className="rounded-full bg-foreground px-1.5 text-[10.5px] font-semibold text-background">failed</span>}
                <span className="font-medium">slurmrestd</span>
                <span className="text-muted-foreground">{health.ping.ok ? `answered in ${health.ping.latency_ms} ms` : health.ping.error ?? `no answer${health.ping.status_code ? ` (${health.ping.status_code})` : ""}`}</span>
              </span>
              <span className="flex items-center gap-2">
                {jwt && jwt.set && !jwt.expired ? <Glyph glyph="on" className="h-[7px] w-[7px]" /> : <span className="rounded-full bg-foreground px-1.5 text-[10.5px] font-semibold text-background">{jwt?.set ? "expired" : "missing"}</span>}
                <span className="font-medium">JWT</span>
                <span className="text-muted-foreground">
                  {!jwt?.set ? "not set" : jwt.expires_at ? `${jwt.expired ? "expired" : "expires"} ${new Date(jwt.expires_at).toLocaleString([], { month: "short", day: "numeric", hour: "numeric", minute: "2-digit" })}` : "valid, no expiry"}
                </span>
              </span>
            </div>
          )}
        </div>

        <SettingsForm id="slurm" sectionOf={sectionOf} labelOf={labelOf} save={save} confirmSave={confirmSave} bar="panel" ariaLabel="Slurm connection">
          <Setting label="Slurm" hint="On, models with a Slurm deployment are launched and kept running." fields={["enabled"]} was={{ field: "enabled", checkbox: true }} className="px-6">
            <Switch name="enabled" label="Launch models on Slurm" checked={enabled} onChange={setEnabled} />
            {!enabled && settings.enabled && <span className="text-xs font-medium">Running jobs keep their nodes; nothing new is launched or replaced.</span>}
          </Setting>
          <Setting label="slurmrestd" hint="The REST daemon's address and the API version it speaks." fields={["url", "api_version"]} className="px-6">
            <div className="flex flex-wrap items-center gap-2">
              <TextField name="url" label="slurmrestd URL" type="url" defaultValue={settings.slurmrestd_url} placeholder="http://slurm:6820" mono className="min-w-0 flex-1" />
              <TextField name="api_version" label="API version" defaultValue={settings.slurmrestd_api_version || "v0.0.40"} placeholder="v0.0.40" mono className="w-28" />
            </div>
          </Setting>
          <Setting label="Sign-in" hint={settings.jwt_set ? `A JWT ending ${settings.jwt_last4 ?? "····"} is set. Paste a new one to replace it; it's never shown.` : "The user jobs run as, and a slurmrestd JWT for it. The JWT is stored encrypted."} fields={["user", "jwt"]} className="px-6">
            <div className="flex flex-wrap items-center gap-2">
              <TextField name="user" label="Slurm user" defaultValue={settings.slurm_user} placeholder="obleth" autoComplete="off" mono className="w-40" />
              <TextField name="jwt" label="Slurm JWT" type="password" autoComplete="new-password" defaultValue="" placeholder={settings.jwt_set ? `•••• ${settings.jwt_last4 ?? ""} (set)` : "Paste the JWT"} mono className="min-w-0 flex-1" />
            </div>
          </Setting>
          <Setting label="Node addresses" hint="Only if obleth's pods can't resolve node names: map each to an IP, and replicas are reached by IP instead of DNS." fields={["aliases"]} className="px-6">
            <input type="hidden" name="aliases" value={JSON.stringify(aliases)} />
            {aliases.map((a, i) => (
              <div key={i} className="flex items-center gap-2">
                <input aria-label="Node name" value={a.host} onChange={(e) => setAliases((x) => x.map((y, j) => (j === i ? { ...y, host: e.target.value } : y)))} placeholder="node001" autoComplete="off" className="h-9 min-w-0 flex-1 rounded-md border border-input bg-background px-3 font-mono text-[12.5px]" />
                <span className="text-muted-foreground">→</span>
                <input aria-label="IP address" value={a.ip} onChange={(e) => setAliases((x) => x.map((y, j) => (j === i ? { ...y, ip: e.target.value } : y)))} placeholder="10.0.0.25" autoComplete="off" className="h-9 w-36 rounded-md border border-input bg-background px-3 font-mono text-[12.5px]" />
                <button type="button" onClick={() => setAliases((x) => x.filter((_, j) => j !== i))} aria-label={`Remove ${a.host || "this address"}`} className="text-xs text-muted-foreground underline underline-offset-[3px] hover:text-foreground">Remove</button>
              </div>
            ))}
            <button type="button" onClick={() => setAliases((x) => [...x, { host: "", ip: "" }])} className="self-start text-[12.5px] text-secondary-foreground underline underline-offset-[3px] hover:text-foreground">{aliases.length ? "Add another" : "Add a node address"}</button>
          </Setting>
        </SettingsForm>
      </Sheet>
    </>
  );
}
