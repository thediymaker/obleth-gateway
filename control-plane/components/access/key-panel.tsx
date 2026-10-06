"use client";

import { useState, useTransition } from "react";
import Link from "next/link";
import { useQuery } from "@tanstack/react-query";
import { deleteKeyAction, replaceKeyAction, saveKeySettingsAction, setKeysDisabledAction } from "@/app/actions";
import { PERIOD_OPTIONS } from "@/components/access/key-dialogs";
import { SettingsForm } from "@/components/access/settings-form";
import { BudgetBar, StateDot } from "@/components/access/ui";
import { SelectField, Setting, Switch, TextField } from "@/components/models/fields";
import { Sheet } from "@/components/models/ui";
import { Pill } from "@/components/overview/ui";
import { Button } from "@/components/ui/button";
import { useConfirm } from "@/components/ui/confirm-dialog";
import { describeIdentityKey } from "@/lib/key-kind";
import { lastUsed, tenantHref, type KeyRow } from "@/lib/access-model";
import { logsHref } from "@/lib/log-links";
import { money } from "@/lib/models-model";
import type { ApiKey, Tenant, UsageDailyRow } from "@/lib/obleth";
import { compact } from "@/lib/overview-model";
import { cn, getJson } from "@/lib/utils";

const DAY_MS = 86_400_000;

function keyFieldSection(name: string): "key" | "tracing" | "end_user" | null {
  if (name === "tracing_enabled") return "tracing";
  if (name === "end_user_fairshare") return "end_user";
  if (["name", "description", "weight", "max_in_flight", "budget_tokens", "budget_cost_usd", "budget_period", "budget_started_at"].includes(name)) return "key";
  return null;
}

const KEY_LABELS: Record<string, string> = {
  name: "Name",
  description: "Description",
  weight: "Weight",
  max_in_flight: "In flight",
  budget_tokens: "Token cap",
  budget_cost_usd: "Spend cap",
  budget_period: "Budget period",
  tracing_enabled: "Trace requests",
  end_user_fairshare: "Per-user fairshare",
};

function Use({ row }: { row: KeyRow }) {
  const k = row.key;
  const start = new Date(Date.now() - 29 * DAY_MS).toISOString().slice(0, 10);
  const daily = useQuery({
    queryKey: ["key-daily", k.id],
    queryFn: () => getJson<UsageDailyRow[]>(`/api/live/usage/daily?key_id=${k.id}&group_by=day&start_day=${start}`),
  });
  const days: { day: string; requests: number; failed: number }[] = [];
  for (let i = 29; i >= 0; i--) {
    const day = new Date(Date.now() - i * DAY_MS).toISOString().slice(0, 10);
    const r = (daily.data ?? []).filter((d) => d.day.slice(0, 10) === day);
    days.push({ day, requests: r.reduce((n, x) => n + Number(x.requests), 0), failed: r.reduce((n, x) => n + Number(x.error_requests), 0) });
  }
  const failed = days.reduce((n, d) => n + d.failed, 0);
  const max = Math.max(1, ...days.map((d) => d.requests));
  const u = row.usage;
  const tiles = [
    { label: "Requests", value: u?.requests ? compact(u.requests) : "0", href: u?.requests ? logsHref({ key: k.id, window: "30d" }) : undefined },
    { label: "Failed", value: daily.data ? compact(failed) : "—", href: failed ? logsHref({ key: k.id, window: "30d", status: "error" }) : undefined },
    { label: "Tokens", value: compact(u?.total_tokens ?? 0) },
    { label: "Spend", value: money(u?.cost_usd ?? 0) },
  ];
  return (
    <section aria-label="Use" className="flex flex-col gap-2.5">
      <div className="flex flex-wrap items-baseline justify-between gap-2">
        <span className="text-[11px] font-semibold uppercase tracking-[0.07em] text-muted-foreground">Use · 30 days</span>
        <span className="text-xs text-muted-foreground">
          {row.lastUsedMs ? `last used ${lastUsed(row.lastUsedMs)}${u?.last_model ? ` on ${u.last_model}` : ""}${u?.last_status_code ? ` · ${u.last_status_code}` : ""}` : "not used in 30 days"}
        </span>
      </div>
      <div className="grid grid-cols-4 gap-2">
        {tiles.map((t) => {
          const body = (
            <>
              <span className="text-[11px] text-muted-foreground">{t.label}</span>
              <span className="text-lg font-semibold tabular-nums">{t.value}</span>
            </>
          );
          const cls = "flex flex-col rounded-lg border border-border px-3 py-2";
          return t.href ? <Link key={t.label} href={t.href} className={cn(cls, "hover:border-muted-foreground/60")}>{body}</Link> : <div key={t.label} className={cls}>{body}</div>;
        })}
      </div>
      <div className="flex h-12 items-end gap-[2px]" aria-hidden="true">
        {days.map((d) => (
          <span key={d.day} title={`${d.day}: ${d.requests} requests${d.failed ? `, ${d.failed} failed` : ""}`} className="flex h-full flex-1 flex-col justify-end">
            {d.failed > 0 && <span className="block rounded-t-[2px] bg-foreground" style={{ height: `${(d.failed / max) * 100}%` }} />}
            <span className={cn("block bg-muted-foreground/70", !d.failed && "rounded-t-[2px]")} style={{ height: `${(Math.max(0, d.requests - d.failed) / max) * 100}%` }} />
          </span>
        ))}
      </div>
    </section>
  );
}

/**
 * One key, over the list: its use, its budget in the budget's own period,
 * its settings with a save bar, and Replace, Move and Delete.
 */
export function KeyPanel({
  row,
  tenants,
  siblings,
  onClose,
  onMove,
  onReplaced,
  onNotice,
  onChanged,
}: {
  row: KeyRow | null;
  tenants: Tenant[];
  /** The other keys in its tenant, for the weight's meaning. */
  siblings: ApiKey[];
  onClose: () => void;
  onMove: (key: ApiKey) => void;
  onReplaced: (created: { secret: string; name: string; detail: string }) => void;
  onNotice: (text: string) => void;
  onChanged: () => void;
}) {
  const { confirm, confirmElement } = useConfirm();
  const [pending, start] = useTransition();
  const [period, setPeriod] = useState(row?.key.budget_period ?? "monthly");
  if (!row) return confirmElement;
  const k = row.key;
  const tenant = tenants.find((t) => t.id === k.tenant_id);
  const identity = k.kind === "identity";
  const busy = siblings.filter((s) => !s.disabled);
  const weightShare = busy.length ? k.weight / (busy.reduce((n, s) => n + s.weight, 0) + k.weight) : 1;

  function toggle() {
    start(async () => {
      if (!k.disabled) {
        const ok = await confirm({ title: `Turn off ${k.name}?`, description: "Requests with it are refused straight away. You can turn it back on.", confirmLabel: "Turn off" });
        if (!ok) return;
      }
      const res = await setKeysDisabledAction([k.id], !k.disabled);
      if (res.failed.length) onNotice(res.failed[0].error);
      onChanged();
    });
  }

  function replace() {
    start(async () => {
      const ok = await confirm({
        title: `Replace ${k.name}?`,
        description: "Makes a new key with the same tenant and settings and shows its secret once. This key keeps working until you turn it off, so a client can switch over without an outage.",
        confirmLabel: "Make the new key",
      });
      if (!ok) return;
      const res = await replaceKeyAction(k.id);
      if (!res.ok) return onNotice(res.error);
      onReplaced({ secret: res.secret, name: res.key.name, detail: `replaces ${k.name} · turn the old one off once the client has switched` });
      onChanged();
    });
  }

  function remove() {
    start(async () => {
      const ok = await confirm({ title: `Delete ${k.name}?`, description: "Requests with it fail at once. Its past usage stays in the logs. This cannot be undone.", confirmLabel: "Delete" });
      if (!ok) return;
      const res = await deleteKeyAction(k.id);
      if (!res.ok) return onNotice(`Delete failed: ${res.error}`);
      onClose();
      onChanged();
    });
  }

  const made = k.created_at ? new Date(k.created_at).toLocaleDateString([], { month: "short", day: "numeric", year: "numeric" }) : null;

  return (
    <>
      {confirmElement}
      <Sheet
        open
        onClose={onClose}
        title={k.name || k.key_prefix}
        width="w-[min(620px,100vw)]"
        description={
          <span className="flex flex-wrap items-center gap-2">
            <Pill><StateDot state={k.disabled ? "off" : "on"} />{k.disabled ? "Off" : "On"}</Pill>
            <Pill>{identity ? "identity key" : "secret key"}</Pill>
            {!identity && <span className="font-mono text-[12px]">{k.key_prefix}…</span>}
            <span>in {tenant ? <Link href={tenantHref(tenant)} className="text-secondary-foreground underline underline-offset-2 hover:text-foreground">{tenant.name}</Link> : "a deleted tenant"}{made ? ` · made ${made}` : ""}</span>
          </span>
        }
      >
        <div className="flex flex-col gap-5 px-6 pb-4">
          <div className="flex flex-wrap gap-2">
            <Button type="button" variant="outline" size="sm" disabled={pending} onClick={toggle}>{k.disabled ? "Turn on" : "Turn off"}</Button>
            {!identity && <Button type="button" variant="outline" size="sm" disabled={pending} onClick={replace}>Replace…</Button>}
            <Button type="button" variant="outline" size="sm" disabled={pending} onClick={() => onMove(k)}>Move…</Button>
            <Button asChild variant="outline" size="sm"><Link href={logsHref({ key: k.id, window: "24h" })}>See its requests</Link></Button>
            <Button type="button" variant="outline" size="sm" disabled={pending} onClick={remove}>Delete…</Button>
          </div>

          {identity && (
            <p className="rounded-lg border border-border px-3 py-2 text-[12.5px] text-secondary-foreground">
              Stands for <span className="font-mono text-[12px] text-foreground">{describeIdentityKey(k).subject}</span> signed in through {describeIdentityKey(k).issuerHost || "their identity provider"}. There is no secret: requests carry their sign-in token.
            </p>
          )}

          <Use row={row} />

          <section aria-label="Budget" className="flex flex-col gap-2">
            <span className="text-[11px] font-semibold uppercase tracking-[0.07em] text-muted-foreground">Budget</span>
            {row.budget ? (
              <div className="flex flex-col gap-2 rounded-lg border border-border px-3.5 py-3">
                <div className="flex flex-wrap items-baseline justify-between gap-2 text-[13px]">
                  <span className="font-medium">{row.budget.label}</span>
                  <span className="text-xs text-muted-foreground">
                    {Math.round(row.budget.share * 100)}%{row.budget.resetsAt ? ` · resets ${row.budget.resetsAt.toLocaleDateString([], { month: "short", day: "numeric" })}` : ""}{row.budget.pace ? ` · ${row.budget.pace}` : ""}
                  </span>
                </div>
                <BudgetBar share={row.budget.share} />
                <p className="text-[11.5px] text-muted-foreground">Counted over the budget&apos;s own period, not the last 30 days.</p>
              </div>
            ) : (
              <p className="text-[12.5px] text-muted-foreground">No budget of its own{tenant?.budget_cost_usd != null || tenant?.budget_tokens != null ? `; ${tenant.name}'s budget still applies` : ""}. Set a cap below.</p>
            )}
          </section>
        </div>

        <SettingsForm
          key={k.id}
          id={k.id}
          sectionOf={keyFieldSection}
          labelOf={(n) => KEY_LABELS[n] ?? n}
          save={saveKeySettingsAction}
          onSaved={onChanged}
          bar="panel"
          ariaLabel={`${k.name} settings`}
        >
          <p className="px-6 pb-1 text-[11px] font-semibold uppercase tracking-[0.07em] text-muted-foreground">Settings</p>
          <input type="hidden" name="budget_started_at" value={k.budget_started_at ?? ""} />
          <Setting label="Name" fields={["name"]} was={{ field: "name" }} className="px-6">
            <TextField name="name" label="Name" required defaultValue={k.name} />
          </Setting>
          <Setting label="Description" fields={["description"]} className="px-6">
            <TextField name="description" label="Description" defaultValue={k.description} placeholder="Optional" />
          </Setting>
          <Setting label="Spend cap" hint="Per period · blank for none" fields={["budget_cost_usd", "budget_period"]} was={{ field: "budget_cost_usd" }} className="px-6">
            <div className="flex flex-wrap items-center gap-2">
              <TextField name="budget_cost_usd" label="Spend cap" inputMode="decimal" defaultValue={k.budget_cost_usd ?? ""} placeholder="No cap" mono className="w-28" />
              <div className="w-36"><SelectField name="budget_period" label="Budget period" value={period} onChange={setPeriod} options={PERIOD_OPTIONS} /></div>
            </div>
          </Setting>
          <Setting label="Token cap" hint="Per period · blank for none" fields={["budget_tokens"]} was={{ field: "budget_tokens" }} className="px-6">
            <TextField name="budget_tokens" label="Token cap" type="number" min={0} step={1} defaultValue={k.budget_tokens ?? ""} placeholder="No cap" mono className="w-40" />
          </Setting>
          <Setting
            label="Weight"
            hint={siblings.length ? `Against ${tenant?.name ?? "its tenant"}'s other ${siblings.length} key${siblings.length === 1 ? "" : "s"}` : "The only key in its tenant"}
            fields={["weight"]}
            was={{ field: "weight" }}
            className="px-6"
          >
            <div className="flex flex-wrap items-center gap-2.5">
              <TextField name="weight" label="Weight" type="number" min={1} step={1} required defaultValue={k.weight} mono className="w-24" />
              {busy.length > 0 && <span className="text-xs text-muted-foreground">{Math.round(weightShare * 100)}% of {tenant?.name ?? "its tenant"}&apos;s slots when all its keys are busy</span>}
            </div>
          </Setting>
          <Setting label="In flight per model" hint="Blank to use the tenant's cap" fields={["max_in_flight"]} was={{ field: "max_in_flight" }} className="px-6">
            <TextField name="max_in_flight" label="In flight per model" type="number" min={1} step={1} defaultValue={k.max_in_flight ?? ""} placeholder={tenant?.max_in_flight ? `tenant's ${tenant.max_in_flight}` : "No cap"} mono className="w-32" />
          </Setting>
          <Setting label="Trace requests" hint="Keep each step of its requests" fields={["tracing_enabled"]} was={{ field: "tracing_enabled", checkbox: true }} className="px-6">
            <Switch name="tracing_enabled" label="Trace requests" defaultChecked={k.tracing_enabled} />
          </Setting>
          <Setting
            label="Per-user fairshare"
            hint="For an app that serves many people through this key: each person it names in a request (x-obleth-end-user or user) waits in line on their own. Only for apps you trust."
            fields={["end_user_fairshare"]}
            was={{ field: "end_user_fairshare", checkbox: true }}
            className="px-6"
          >
            <Switch name="end_user_fairshare" label="Per-user fairshare" defaultChecked={k.end_user_fairshare} />
          </Setting>
        </SettingsForm>
      </Sheet>
    </>
  );
}
