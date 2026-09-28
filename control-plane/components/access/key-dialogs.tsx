"use client";

import { useState, useTransition } from "react";
import Link from "next/link";
import { createKeyAction, moveKeysAction, setKeysBudgetAction, type BulkKeyResult } from "@/app/actions";
import { Field, SelectField } from "@/components/models/fields";
import { Sheet } from "@/components/models/ui";
import { Segmented } from "@/components/overview/ui";
import { Button } from "@/components/ui/button";
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { Select } from "@/components/ui/select";
import { tenantHref } from "@/lib/access-model";
import type { ApiKey, Tenant } from "@/lib/obleth";

export const PERIOD_OPTIONS = [
  { value: "monthly", label: "each month" },
  { value: "term", label: "per term" },
  { value: "lifetime", label: "in total" },
];

function failures(r: BulkKeyResult): string {
  return r.failed.length ? ` ${r.failed.length} failed: ${r.failed.map((f) => `${f.name} (${f.error})`).join("; ")}` : "";
}

/**
 * Move keys to another tenant, or to a new one made for them. Moving a heavy
 * user into a tenant of their own gives them their own share, limits and
 * budget without a new secret.
 */
export function MoveDialog({
  open,
  onClose,
  keys,
  tenants,
  onDone,
}: {
  open: boolean;
  onClose: () => void;
  keys: ApiKey[];
  tenants: Tenant[];
  onDone: (text: string) => void;
}) {
  const from = [...new Set(keys.map((k) => k.tenant_id))];
  const [mode, setMode] = useState<"existing" | "new">("existing");
  const [target, setTarget] = useState("");
  const [name, setName] = useState("");
  const [copyFrom, setCopyFrom] = useState(from.length === 1 ? from[0] : "");
  const [error, setError] = useState<string | null>(null);
  const [pending, start] = useTransition();
  const [made, setMade] = useState<Tenant | { id: string; name: string } | null>(null);
  const others = tenants.filter((t) => !(from.length === 1 && t.id === from[0]));
  const label = keys.length === 1 ? (keys[0].name || keys[0].key_prefix) : `${keys.length} keys`;
  const fromName = from.length === 1 ? tenants.find((t) => t.id === from[0])?.name : null;

  function submit() {
    setError(null);
    if (mode === "existing" && !target) return setError("Pick the tenant to move to.");
    if (mode === "new" && !name.trim()) return setError("Name the new tenant.");
    start(async () => {
      const res = await moveKeysAction(keys.map((k) => k.id), mode === "existing" ? { tenantId: target } : { newTenant: { name: name.trim(), copyFrom: copyFrom || undefined } });
      if (res.error) return setError(res.error);
      const dest = mode === "existing" ? tenants.find((t) => t.id === target)?.name ?? "the tenant" : name.trim();
      onDone(`Moved ${res.done} key${res.done === 1 ? "" : "s"} to ${dest}.${failures(res)}`);
      if (mode === "new" && res.tenantId) setMade({ id: res.tenantId, name: name.trim() });
      else onClose();
    });
  }

  return (
    <Dialog open={open} onOpenChange={(v) => { if (!v) { setMade(null); onClose(); } }}>
      <DialogContent className="max-w-lg">
        <DialogHeader>
          <DialogTitle>Move {label}</DialogTitle>
          <DialogDescription>
            {fromName ? `Out of ${fromName}. ` : ""}Secrets keep working. From now on {keys.length === 1 ? "it uses" : "they use"} the new tenant&apos;s share, limits, hours and budget; past usage stays with the tenant it was recorded under.
          </DialogDescription>
        </DialogHeader>
        {made ? (
          <div className="flex flex-col gap-3 text-[13px]">
            <p>{made.name} is made and the keys are in it.</p>
            <p className="text-muted-foreground">Set its weight, limits and budget on its page.</p>
            <DialogFooter>
              <Button type="button" variant="outline" onClick={() => { setMade(null); onClose(); }}>Close</Button>
              <Button asChild><Link href={tenantHref(made)}>Open {made.name}</Link></Button>
            </DialogFooter>
          </div>
        ) : (
          <div className="flex flex-col gap-4">
            <Segmented label="Move to" value={mode} onChange={setMode} options={[{ value: "existing", label: "A tenant that exists" }, { value: "new", label: "A new tenant" }]} />
            {mode === "existing" ? (
              <div className="space-y-1.5">
                <span className="text-[12.5px] font-medium text-secondary-foreground">Tenant</span>
                <Select aria-label="Tenant to move to" value={target} onValueChange={setTarget} searchPlaceholder="Find a tenant" className="h-9 text-[13px]" options={[{ value: "", label: "Pick a tenant" }, ...others.map((t) => ({ value: t.id, label: t.name }))]} />
              </div>
            ) : (
              <>
                <Field label="Name" name="new_tenant_name" value={name} onChange={(e) => setName(e.target.value)} placeholder={keys.length === 1 ? `${keys[0].name}-own` : "heavy-users"} hint="A tenant of their own: its own share of the gateway, limits and budget." />
                <div className="space-y-1.5">
                  <span className="text-[12.5px] font-medium text-secondary-foreground">Start with the settings of</span>
                  <Select aria-label="Copy settings from" value={copyFrom} onValueChange={setCopyFrom} className="h-9 text-[13px]" options={[{ value: "", label: "Nothing: defaults, no limits" }, ...tenants.map((t) => ({ value: t.id, label: t.name }))]} />
                  <p className="text-[11.5px] leading-snug text-muted-foreground">Copies weight, group, limits, hours, budget, models, guardrails and compression. You can change any of them after.</p>
                </div>
              </>
            )}
            {error && <p role="alert" className="text-[12.5px] font-medium text-foreground">{error}</p>}
            <DialogFooter>
              <Button type="button" variant="outline" onClick={onClose} disabled={pending}>Cancel</Button>
              <Button type="button" onClick={submit} disabled={pending}>{pending ? "Moving…" : mode === "new" ? "Make it and move" : "Move"}</Button>
            </DialogFooter>
          </div>
        )}
      </DialogContent>
    </Dialog>
  );
}

/** One budget for every chosen key. Blank caps take the budget off. */
export function BudgetDialog({ open, onClose, keys, onDone }: { open: boolean; onClose: () => void; keys: ApiKey[]; onDone: (text: string) => void }) {
  const [cost, setCost] = useState("");
  const [tokens, setTokens] = useState("");
  const [period, setPeriod] = useState("monthly");
  const [error, setError] = useState<string | null>(null);
  const [pending, start] = useTransition();
  function submit() {
    const c = cost.trim() === "" ? null : Number(cost);
    const t = tokens.trim() === "" ? null : Number(tokens);
    if ((c !== null && !(c >= 0)) || (t !== null && !(Number.isInteger(t) && t >= 0))) return setError("Caps are numbers of 0 or more, or blank for none.");
    start(async () => {
      const res = await setKeysBudgetAction(keys.map((k) => k.id), { budget_cost_usd: c, budget_tokens: t, budget_period: period });
      onDone(`${c === null && t === null ? "Took the budget off" : "Set the budget on"} ${res.done} key${res.done === 1 ? "" : "s"}.${failures(res)}`);
      onClose();
    });
  }
  return (
    <Dialog open={open} onOpenChange={(v) => { if (!v) onClose(); }}>
      <DialogContent className="max-w-md">
        <DialogHeader>
          <DialogTitle>Set a budget on {keys.length} key{keys.length === 1 ? "" : "s"}</DialogTitle>
          <DialogDescription>Replaces each key&apos;s own budget. A key stops once it reaches a cap, until the period resets. Leave both blank to take budgets off.</DialogDescription>
        </DialogHeader>
        <div className="grid gap-3 sm:grid-cols-2">
          <Field label="Spend cap, US dollars" name="bulk_budget_cost" inputMode="decimal" value={cost} onChange={(e) => setCost(e.target.value)} placeholder="No cap" />
          <Field label="Token cap" name="bulk_budget_tokens" inputMode="numeric" value={tokens} onChange={(e) => setTokens(e.target.value)} placeholder="No cap" />
        </div>
        <div className="space-y-1.5">
          <span className="text-[12.5px] font-medium text-secondary-foreground">Period</span>
          <SelectField label="Budget period" value={period} onChange={setPeriod} options={PERIOD_OPTIONS} />
        </div>
        {error && <p role="alert" className="text-[12.5px] font-medium text-foreground">{error}</p>}
        <DialogFooter>
          <Button type="button" variant="outline" onClick={onClose} disabled={pending}>Cancel</Button>
          <Button type="button" onClick={submit} disabled={pending}>{pending ? "Saving…" : "Set budget"}</Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

/** A new secret key: its tenant, name and optional limits. */
export function NewKeySheet({
  open,
  onClose,
  tenants,
  tenantId,
  onCreated,
}: {
  open: boolean;
  onClose: () => void;
  tenants: Tenant[];
  tenantId: string;
  onCreated: (created: { secret: string; name: string; tenantName: string; detail: string }) => void;
}) {
  const [tenant, setTenant] = useState(tenantId);
  const [period, setPeriod] = useState("monthly");
  const [error, setError] = useState<string | null>(null);
  const [pending, start] = useTransition();
  const active = tenants.filter((t) => t.status === "active" || t.id === tenantId);

  return (
    <Sheet
      open={open}
      onClose={onClose}
      title="New key"
      description="A secret for one client or person. It's shown once, when it's made."
      width="w-[min(520px,100vw)]"
      footer={
        <div className="flex items-center justify-between gap-3">
          <span role="alert" className="text-[12.5px] font-medium">{error}</span>
          <div className="flex gap-2">
            <Button type="button" variant="outline" size="sm" onClick={onClose} disabled={pending}>Cancel</Button>
            <Button type="submit" form="new-key-form" size="sm" disabled={pending}>{pending ? "Making…" : "Make key"}</Button>
          </div>
        </div>
      }
    >
      <form
        id="new-key-form"
        className="flex flex-col gap-4 px-6 pb-6"
        onSubmit={(e) => {
          e.preventDefault();
          const fd = new FormData(e.currentTarget);
          if (!tenant) return setError("Pick its tenant.");
          fd.set("tenant_id", tenant);
          setError(null);
          start(async () => {
            const res = await createKeyAction(fd);
            if (!res.ok || !res.secret) return setError(res.ok ? "The gateway didn't return a secret." : res.error);
            const cost = String(fd.get("budget_cost_usd") ?? "").trim();
            const tokens = String(fd.get("budget_tokens") ?? "").trim();
            const words = PERIOD_OPTIONS.find((p) => p.value === period)?.label ?? "";
            onCreated({
              secret: res.secret,
              name: String(fd.get("name") ?? ""),
              tenantName: tenants.find((t) => t.id === tenant)?.name ?? "",
              detail: [`weight ${fd.get("weight") || 100}`, cost ? `$${cost} ${words}` : tokens ? `${tokens} tokens ${words}` : "no budget"].join(" · "),
            });
          });
        }}
      >
        <div className="space-y-1.5">
          <span className="text-[12.5px] font-medium text-secondary-foreground">Tenant</span>
          <Select aria-label="Tenant" value={tenant} onValueChange={setTenant} searchPlaceholder="Find a tenant" className="h-9 text-[13px]" options={[{ value: "", label: "Pick a tenant" }, ...active.map((t) => ({ value: t.id, label: t.name }))]} />
          <p className="text-[11.5px] text-muted-foreground">Its share, limits, hours and models apply to the key.</p>
        </div>
        <Field label="Name" name="name" required placeholder="What or who it's for" />
        <Field label="Description" name="description" placeholder="Optional" />
        <div className="grid gap-3 sm:grid-cols-2">
          <Field label="Weight" name="weight" type="number" min={1} step={1} defaultValue="100" hint="Its share against the tenant's other keys." />
          <Field label="In flight per model" name="max_in_flight" type="number" min={1} step={1} placeholder="The tenant's" hint="Blank to use the tenant's cap." />
        </div>
        <div className="grid gap-3 sm:grid-cols-2">
          <Field label="Spend cap, US dollars" name="budget_cost_usd" inputMode="decimal" placeholder="No cap" />
          <Field label="Token cap" name="budget_tokens" type="number" min={0} step={1} placeholder="No cap" />
        </div>
        <div className="space-y-1.5">
          <span className="text-[12.5px] font-medium text-secondary-foreground">Budget period</span>
          <SelectField name="budget_period" label="Budget period" value={period} onChange={setPeriod} options={PERIOD_OPTIONS} />
        </div>
      </form>
    </Sheet>
  );
}
