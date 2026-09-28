"use client";

import { useCallback, useMemo, useState } from "react";
import { saveTenantSettingsAction, type TenantSettingsSection } from "@/app/actions";
import { CompressionFields, GuardrailsFields, guardrailsProblem } from "@/components/access/policy-fields";
import { SettingsForm } from "@/components/access/settings-form";
import { BudgetBar, SettingsCard } from "@/components/access/ui";
import { WeekGrid } from "@/components/access/week-grid";
import { SelectField, Setting, Switch, TextArea, TextField } from "@/components/models/fields";
import { clockIn, groupShare, isTimeZone, utcToZoned, zonedToUtc, type BudgetView } from "@/lib/access-model";
import { money } from "@/lib/models-model";
import type { Tenant } from "@/lib/obleth";
import { cn } from "@/lib/utils";

/** The save call that owns each field; null for fields that never save. */
export function tenantFieldSection(name: string): TenantSettingsSection | null {
  if (["name", "organization", "description", "contact_email"].includes(name)) return "profile";
  if (name === "fairshare_group") return "group";
  if (name === "weight") return "weight";
  if (name === "tokens_per_minute" || name === "max_in_flight") return "quota";
  if (name === "tracing_enabled") return "tracing";
  if (name === "synthetic") return "synthetic";
  if (["timezone", "weekly_windows", "active_from", "active_until"].includes(name)) return "schedule";
  if (name.startsWith("budget_")) return "budget";
  if (name === "allow_all" || name === "allowed_model") return "allowlist";
  if (name === "guardrails_policy") return "guardrails";
  if (name === "compression_policy") return "compression";
  return null;
}

const LABELS: Record<string, string> = {
  name: "Name",
  organization: "Organization",
  description: "Description",
  contact_email: "Contact",
  fairshare_group: "Fairshare group",
  weight: "Weight",
  tokens_per_minute: "Token rate",
  max_in_flight: "In flight",
  tracing_enabled: "Tracing",
  synthetic: "Synthetic",
  timezone: "Time zone",
  weekly_windows: "Weekly hours",
  active_from: "Open from",
  active_until: "Open until",
  budget_cost_usd: "Spend cap",
  budget_tokens: "Token cap",
  budget_period: "Budget period",
  budget_restart: "New budget period",
  allow_all: "Models",
  allowed_model: "Models",
  guardrails_policy: "Guardrails",
  compression_policy: "Compression",
};

export const tenantFieldLabel = (name: string) => LABELS[name] ?? name.replace(/_/g, " ");

/** Which card on the page each save section lives in, for the section list's dots. */
export const SECTION_CARD: Record<TenantSettingsSection, string> = {
  profile: "profile",
  group: "limits",
  weight: "limits",
  quota: "limits",
  schedule: "hours",
  budget: "budget",
  allowlist: "models",
  guardrails: "guardrails",
  compression: "compression",
  tracing: "advanced",
  synthetic: "advanced",
};

const PERIODS = [
  { value: "monthly", label: "each month" },
  { value: "term", label: "per term" },
  { value: "lifetime", label: "in total" },
];

const ZONES: string[] = (() => {
  try {
    return (Intl as unknown as { supportedValuesOf?: (k: string) => string[] }).supportedValuesOf?.("timeZone") ?? [];
  } catch {
    return [];
  }
})();

function WeightField({ tenant, tenants, group }: { tenant: Tenant; tenants: Tenant[]; group: string }) {
  const [weight, setWeight] = useState(String(tenant.weight));
  const w = Math.max(1, Math.round(Number(weight) || 1));
  const before = groupShare(tenants, tenant.id, tenant.fairshare_group, tenant.weight);
  const after = groupShare(tenants, tenant.id, group, w);
  const peers = tenants.filter((t) => t.id !== tenant.id && t.fairshare_group === group && t.status === "active");
  const pct = (x: number) => `${Math.round(x * 100)}%`;
  return (
    <Setting id="set-weight" label="Weight" hint="Its share against the other tenants in the group." fields={["weight"]} was={{ field: "weight" }}>
      <div className="flex flex-wrap items-center gap-3">
        <input type="range" min={1} max={Math.max(1000, w)} step={1} value={w} onChange={(e) => setWeight(e.target.value)} aria-label="Weight slider" className="w-full max-w-[320px] accent-foreground" />
        <TextField name="weight" label="Weight" type="number" min={1} step={1} required value={weight} onChange={(e) => setWeight(e.target.value)} mono className="w-24" />
      </div>
      <p className="text-[12.5px] text-secondary-foreground">
        When everyone in <span className="font-mono text-[12px]">{group}</span> is busy: <b className="font-medium text-foreground">{pct(after)}</b> of its slots
        {group === tenant.fairshare_group && w !== tenant.weight ? `, ${after > before ? "up" : "down"} from ${pct(before)}` : ""}
        {peers.length ? ` alongside ${peers.length} other tenant${peers.length === 1 ? "" : "s"}.` : ", the only active tenant there."}
      </p>
    </Setting>
  );
}

function DateRange({ tenant, zone }: { tenant: Tenant; zone: string }) {
  const [from, setFrom] = useState<string | null>(null);
  const [until, setUntil] = useState<string | null>(null);
  const shownFrom = from ?? utcToZoned(tenant.active_from, zone);
  const shownUntil = until ?? utcToZoned(tenant.active_until, zone);
  const iso = (local: string | null, stored: string | null) => (local === null ? stored ?? "" : local ? zonedToUtc(local, zone) ?? "" : "");
  const end = tenant.active_until && until === null ? new Date(tenant.active_until) : shownUntil ? new Date(zonedToUtc(shownUntil, zone) ?? "") : null;
  const daysLeft = end && !Number.isNaN(end.getTime()) ? Math.ceil((end.getTime() - Date.now()) / 86_400_000) : null;
  return (
    <Setting id="set-dates" label="Open from · until" hint="A term or a trial: keys stop working after the end. Blank for no limit." fields={["active_from", "active_until"]}>
      <input type="hidden" name="active_from" value={iso(from, tenant.active_from)} />
      <input type="hidden" name="active_until" value={iso(until, tenant.active_until)} />
      <div className="flex flex-wrap items-center gap-2">
        <TextField name="active_from_local" label="Open from" type="datetime-local" value={shownFrom} onChange={(e) => setFrom(e.target.value)} className="w-[200px]" />
        <span className="text-muted-foreground">to</span>
        <TextField name="active_until_local" label="Open until" type="datetime-local" value={shownUntil} onChange={(e) => setUntil(e.target.value)} className="w-[200px]" />
        {daysLeft !== null && <span className="text-xs text-muted-foreground">{daysLeft > 0 ? `${daysLeft} days left` : "ended"}</span>}
      </div>
    </Setting>
  );
}

function BudgetFields({ tenant, budget }: { tenant: Tenant; budget: (BudgetView & { usedCost: number; usedTokens: number }) | null }) {
  const [cost, setCost] = useState(tenant.budget_cost_usd == null ? "" : String(tenant.budget_cost_usd));
  const [tokens, setTokens] = useState(tenant.budget_tokens == null ? "" : String(tenant.budget_tokens));
  const [period, setPeriod] = useState(tenant.budget_period ?? "monthly");
  const capCost = Number(cost);
  const capTokens = Number(tokens);
  const share = budget ? Math.max(cost && capCost > 0 ? budget.usedCost / capCost : 0, tokens && capTokens > 0 ? budget.usedTokens / capTokens : 0) : null;
  const periodWord = period === "monthly" ? "this month" : period === "term" ? "this term" : "so far";
  return (
    <>
      <Setting id="set-budget" label="Spend cap" hint="US dollars per period. Blank for no cap." fields={["budget_cost_usd", "budget_period"]} was={{ field: "budget_cost_usd" }}>
        <div className="flex flex-wrap items-center gap-2">
          <span className="flex h-9 w-32 items-center rounded-md border border-input bg-background pl-3 text-[13px] focus-within:ring-1 focus-within:ring-ring">
            <span className="text-muted-foreground">$</span>
            <input name="budget_cost_usd" aria-label="Spend cap" inputMode="decimal" value={cost} onChange={(e) => setCost(e.target.value)} placeholder="No cap" className="h-full min-w-0 flex-1 bg-transparent px-1.5 font-mono text-[12.5px] outline-none" />
          </span>
          <div className="w-36"><SelectField name="budget_period" label="Budget period" value={period} onChange={setPeriod} options={PERIODS} /></div>
        </div>
        {budget && (cost || tokens) && (
          <div className="flex flex-wrap items-center gap-2.5 text-[12.5px] text-secondary-foreground">
            <BudgetBar share={share} className="w-[200px]" />
            {money(budget.usedCost)} used {periodWord}
            {share !== null && ` · ${Math.round(share * 100)}% of ${cost && tenant.budget_cost_usd !== capCost ? "the new cap" : "the cap"}`}
            {budget.pace && ` · ${budget.pace}`}
          </div>
        )}
      </Setting>
      <Setting id="set-token-cap" label="Token cap" hint="Tokens per period. Blank for no cap." fields={["budget_tokens"]} was={{ field: "budget_tokens" }}>
        <TextField name="budget_tokens" label="Token cap" type="number" min={0} step={1} value={tokens} onChange={(e) => setTokens(e.target.value)} placeholder="No cap" mono className="w-44" />
      </Setting>
      {period !== "monthly" && (cost || tokens) && (
        <Setting id="set-budget-restart" label="Start a new period" hint={tenant.budget_started_at ? `This one began ${new Date(tenant.budget_started_at).toLocaleDateString()}.` : "Counting starts now."} fields={["budget_restart"]}>
          <Switch name="budget_restart" label="Start counting again from now" defaultChecked={false} />
        </Setting>
      )}
    </>
  );
}

function ModelsField({ tenant, models }: { tenant: Tenant; models: string[] }) {
  const stored = tenant.allowed_models ?? [];
  const [all, setAll] = useState(stored.length === 0);
  const [picked, setPicked] = useState<string[]>(stored);
  const [find, setFind] = useState("");
  const names = [...new Set([...models, ...stored])].sort();
  const shown = names.filter((m) => m.toLowerCase().includes(find.trim().toLowerCase()));
  return (
    <Setting id="set-models" label="Models it can use" hint={all ? `All ${models.length}, including models added later.` : `${picked.length} of ${models.length}. Others are refused.`} fields={["allow_all", "allowed_model"]}>
      <input type="hidden" name="has_allowlist" value="1" />
      <Switch name="allow_all" label="All models" checked={all} onChange={setAll}>All models</Switch>
      {!all && (
        <>
          {picked.map((m) => <input key={m} type="hidden" name="allowed_model" value={m} />)}
          <div className="flex flex-wrap items-center gap-2">
            <input value={find} onChange={(e) => setFind(e.target.value)} aria-label="Find a model" placeholder="Find a model" className="h-8 w-56 rounded-md border border-input bg-background px-2.5 text-[12.5px] outline-none focus-visible:ring-1 focus-visible:ring-ring" />
            <button type="button" className="text-[12px] text-muted-foreground hover:text-foreground" onClick={() => setPicked([...new Set([...picked, ...shown])])}>Pick {find ? "these" : "all"}</button>
            <button type="button" className="text-[12px] text-muted-foreground hover:text-foreground" onClick={() => setPicked(picked.filter((m) => !shown.includes(m)))}>Clear {find ? "these" : "all"}</button>
          </div>
          <div className="flex max-h-56 flex-wrap gap-1.5 overflow-y-auto">
            {shown.map((m) => {
              const on = picked.includes(m);
              return (
                <button
                  key={m}
                  type="button"
                  aria-pressed={on}
                  onClick={() => setPicked(on ? picked.filter((x) => x !== m) : [...picked, m])}
                  className={cn("inline-flex h-7 items-center rounded-full border px-2.5 font-mono text-[11.5px]", on ? "border-muted-foreground bg-secondary text-foreground" : "border-border text-muted-foreground hover:text-foreground")}
                >
                  {m}
                </button>
              );
            })}
          </div>
          {picked.length === 0 && <p className="text-xs text-foreground">None picked: every request would be refused. Pick at least one, or turn on All models.</p>}
        </>
      )}
    </Setting>
  );
}

function ScheduleFields({ tenant }: { tenant: Tenant }) {
  const [zone, setZone] = useState(tenant.timezone || "UTC");
  const zones = useMemo(() => {
    const list = ZONES.length ? ZONES : ["UTC"];
    return [...new Set([tenant.timezone || "UTC", ...list])].map((z) => ({ value: z, label: z }));
  }, [tenant.timezone]);
  const valid = isTimeZone(zone) ? zone : "UTC";
  return (
    <>
      <Setting id="set-timezone" label="Time zone" hint="Hours and dates below are in this zone." fields={["timezone"]} was={{ field: "timezone" }}>
        <div className="flex flex-wrap items-center gap-2.5">
          <div className="w-72"><SelectField name="timezone" label="Time zone" value={zone} onChange={setZone} options={zones} /></div>
          <span className="text-xs text-muted-foreground">it is {clockIn(valid)} there now</span>
        </div>
      </Setting>
      <Setting id="set-hours" label="Weekly hours" hint="Press and drag across the grid to open or close hours. Nothing open means always open." fields={["weekly_windows"]}>
        <WeekGrid name="weekly_windows" windows={tenant.weekly_windows} />
      </Setting>
      <DateRange tenant={tenant} zone={valid} />
    </>
  );
}

/**
 * Every setting a tenant has, as one form with one save bar. Each changed
 * part saves through the call that owns it.
 */
export function TenantSettings({
  tenant,
  tenants,
  models,
  budget,
  onDirty,
  onSaved,
}: {
  tenant: Tenant;
  tenants: Tenant[];
  models: string[];
  budget: (BudgetView & { usedCost: number; usedTokens: number }) | null;
  onDirty: (cards: string[]) => void;
  onSaved: (data: FormData) => void;
}) {
  const [group, setGroup] = useState(tenant.fairshare_group);
  const groups = [...new Set(tenants.map((t) => t.fairshare_group))].sort();
  const onDirtyChange = useCallback((sections: string[]) => onDirty([...new Set(sections.map((s) => SECTION_CARD[s as TenantSettingsSection]))]), [onDirty]);
  const validate = useCallback((data: FormData) => guardrailsProblem(String(data.get("guardrails_policy") ?? "")), []);

  return (
    <SettingsForm
      id={tenant.id}
      sectionOf={tenantFieldSection}
      labelOf={tenantFieldLabel}
      save={saveTenantSettingsAction}
      validate={validate}
      onSaved={onSaved}
      onDirtyChange={onDirtyChange}
      className="flex flex-col gap-4"
      ariaLabel={`${tenant.name} settings`}
    >
      <SettingsCard id="profile" title="Profile" description="Who it is, and who to ask about it.">
        <Setting id="set-name" label="Name" hint="Shown everywhere, and in this page's address." fields={["name"]} was={{ field: "name" }}>
          <TextField name="name" label="Name" required defaultValue={tenant.name} className="max-w-sm" />
        </Setting>
        <Setting id="set-organization" label="Organization" fields={["organization"]} was={{ field: "organization" }}>
          <TextField name="organization" label="Organization" defaultValue={tenant.organization} placeholder="Department or group" className="max-w-sm" />
        </Setting>
        <Setting id="set-description" label="Description" hint="What it's for." fields={["description"]}>
          <TextArea name="description" label="Description" rows={2} defaultValue={tenant.description} placeholder="What this tenant is for" className="font-sans text-[13px]" />
        </Setting>
        <Setting id="set-contact" label="Contact" hint="Who to write to about its use." fields={["contact_email"]} was={{ field: "contact_email" }}>
          <TextField name="contact_email" label="Contact email" type="email" defaultValue={tenant.contact_email} placeholder="name@asu.edu" className="max-w-sm" />
        </Setting>
      </SettingsCard>

      <SettingsCard id="limits" title="Limits" description="How much of the gateway it gets when it's busy, and its hard caps.">
        <Setting id="set-group" label="Fairshare group" hint="Tenants in a group share its slots by weight." fields={["fairshare_group"]} was={{ field: "fairshare_group" }}>
          <div className="flex flex-wrap items-center gap-2.5">
            <TextField name="fairshare_group" label="Fairshare group" required maxLength={64} list="fairshare-groups" value={group} onChange={(e) => setGroup(e.target.value.trim() || e.target.value)} mono className="w-56" />
            <datalist id="fairshare-groups">{groups.map((g) => <option key={g} value={g} />)}</datalist>
            <span className="text-xs text-muted-foreground">
              {groups.includes(group) ? `${tenants.filter((t) => t.fairshare_group === group).length} tenants · type a new name to start a group` : "a new group, made at weight 100 when you save"}
            </span>
          </div>
        </Setting>
        <WeightField tenant={tenant} tenants={tenants} group={group} />
        <Setting id="set-rate" label="Token rate" hint="Tokens a minute across all its keys. Blank or 0 for no limit." fields={["tokens_per_minute"]} was={{ field: "tokens_per_minute" }}>
          <TextField name="tokens_per_minute" label="Token rate" type="number" min={0} step={1} defaultValue={tenant.tokens_per_minute || ""} placeholder="No limit" mono className="w-40" />
        </Setting>
        <Setting id="set-in-flight" label="In flight per model" hint="Requests it can run at once on any one model. Blank for no cap." fields={["max_in_flight"]} was={{ field: "max_in_flight" }}>
          <TextField name="max_in_flight" label="In flight per model" type="number" min={1} step={1} defaultValue={tenant.max_in_flight ?? ""} placeholder="No cap" mono className="w-28" />
        </Setting>
      </SettingsCard>

      <SettingsCard id="hours" title="Access hours" description="When its keys work. Outside these hours requests are refused with a clear error.">
        <ScheduleFields tenant={tenant} />
      </SettingsCard>

      <SettingsCard id="budget" title="Budget" description="Requests are refused once a cap is reached, until the period resets.">
        <BudgetFields tenant={tenant} budget={budget} />
      </SettingsCard>

      <SettingsCard id="models" title="Models" description="Which models its keys can call.">
        <ModelsField tenant={tenant} models={models} />
      </SettingsCard>

      <SettingsCard id="guardrails" title="Guardrails" description="Screen its requests and responses in the gateway, for every one of its keys.">
        <GuardrailsFields policy={tenant.guardrails_policy} models={models} />
      </SettingsCard>

      <SettingsCard id="compression" title="Compression" description="Shrink what its requests send to the model before dispatch.">
        <CompressionFields policy={tenant.compression_policy} />
      </SettingsCard>

      <SettingsCard id="advanced" title="Advanced">
        <Setting id="set-tracing" label="Trace requests" hint="Keep each step of every request, for the request panel's timeline." fields={["tracing_enabled"]} was={{ field: "tracing_enabled", checkbox: true }}>
          <Switch name="tracing_enabled" label="Trace requests" defaultChecked={tenant.tracing_enabled} />
        </Setting>
        <Setting id="set-synthetic" label="Synthetic traffic" hint="Benchmark and test traffic: its requests are tagged as benchmark and left out of usage and cost stats by default." fields={["synthetic"]} was={{ field: "synthetic", checkbox: true }}>
          <Switch name="synthetic" label="Synthetic traffic" defaultChecked={tenant.synthetic} />
        </Setting>
      </SettingsCard>
    </SettingsForm>
  );
}
