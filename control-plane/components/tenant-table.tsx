"use client";

import { Fragment, useState, useTransition } from "react";
import { ChevronDown, Plus, Trash2, X } from "lucide-react";
import {
  deleteTenantAction,
  setTenantAllowlistAction,
  setTenantBudgetAction,
  setTenantScheduleAction,
  setTenantStatusAction,
  updateTenantAction,
} from "@/app/actions";
import { QuotaControl } from "@/components/quota-control";
import { WeightControl } from "@/components/weight-control";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import type { Tenant, WeeklyWindow } from "@/lib/obleth";
import { cn } from "@/lib/utils";

const STATUS_STYLES: Record<string, string> = {
  active: "border-emerald-500/40 text-emerald-500",
  suspended: "border-amber-500/40 text-amber-500",
  archived: "border-muted-foreground/40 text-muted-foreground",
};

const DAY_LABELS = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

const BUDGET_PERIODS = ["lifetime", "monthly", "term"] as const;

/** Short badge label summarising a tenant's budget caps. */
function budgetLabel(t: Tenant): string {
  const period = t.budget_period ?? "lifetime";
  const parts: string[] = [];
  if (t.budget_tokens != null) parts.push(`${formatCompact(t.budget_tokens)} tok`);
  if (t.budget_cost_usd != null) parts.push(`$${t.budget_cost_usd}`);
  return `${parts.join(" / ")} ${period}`.trim();
}

/** Compact human number (e.g. 1.2M, 15K). */
function formatCompact(n: number): string {
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(n % 1_000_000 === 0 ? 0 : 1)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(n % 1_000 === 0 ? 0 : 1)}K`;
  return String(n);
}

type ScheduleBadge = { label: string; className: string };

/** Compute the live schedule state of a tenant, evaluated in its timezone. */
function scheduleBadge(t: Tenant): ScheduleBadge | null {
  const hasSchedule = t.active_from || t.active_until || (t.weekly_windows?.length ?? 0) > 0;
  if (!hasSchedule) return null;
  const now = new Date();
  if (t.active_from && now < new Date(t.active_from)) {
    return { label: "Scheduled", className: "border-sky-500/40 text-sky-500" };
  }
  if (t.active_until && now >= new Date(t.active_until)) {
    return { label: "Expired", className: "border-muted-foreground/40 text-muted-foreground" };
  }
  if (t.weekly_windows && t.weekly_windows.length > 0) {
    let local: Date;
    try {
      local = new Date(now.toLocaleString("en-US", { timeZone: t.timezone || "UTC" }));
    } catch {
      local = now;
    }
    const day = local.getDay();
    const minute = local.getHours() * 60 + local.getMinutes();
    const open = t.weekly_windows.some(
      (w) => w.day === day && minute >= w.start_min && minute < w.end_min,
    );
    return open
      ? { label: "In window", className: "border-emerald-500/40 text-emerald-500" }
      : { label: "Outside window", className: "border-amber-500/40 text-amber-500" };
  }
  return { label: "In window", className: "border-emerald-500/40 text-emerald-500" };
}

export function TenantTable({ tenants, models }: { tenants: Tenant[]; models: string[] }) {
  const [expandedId, setExpandedId] = useState<string | null>(null);
  const [pending, start] = useTransition();

  return (
    <table className="w-full text-sm">
      <thead>
        <tr className="border-b border-border text-left text-xs text-muted-foreground">
          <th className="px-6 py-3 font-medium">Name</th>
          <th className="px-3 py-3 font-medium">Status</th>
          <th className="px-3 py-3 font-medium">Quotas</th>
          <th className="px-3 py-3 font-medium">Fairshare weight</th>
          <th className="px-3 py-3 text-right font-medium">Edit</th>
        </tr>
      </thead>
      <tbody>
        {tenants.map((t) => {
          const expanded = expandedId === t.id;
          const subline = [t.organization, t.description].filter(Boolean).join(" — ");
          return (
            <Fragment key={t.id}>
              <tr className="border-b border-border/60 align-top">
                <td className="px-6 py-3">
                  <div className="font-medium">{t.name}</div>
                  {subline && <div className="text-xs text-muted-foreground">{subline}</div>}
                </td>
                <td className="px-3 py-3">
                  <div className="flex flex-wrap items-center gap-1.5">
                    <Badge className={STATUS_STYLES[t.status] ?? STATUS_STYLES.archived}>{t.status}</Badge>
                    {(() => {
                      const sb = scheduleBadge(t);
                      return sb ? <Badge className={sb.className}>{sb.label}</Badge> : null;
                    })()}
                    {(t.budget_tokens != null || t.budget_cost_usd != null) && (
                      <Badge className="border-violet-500/40 text-violet-500">{budgetLabel(t)}</Badge>
                    )}
                    {t.allowed_models && t.allowed_models.length > 0 && (
                      <Badge className="border-indigo-500/40 text-indigo-500">
                        {t.allowed_models.length} model{t.allowed_models.length === 1 ? "" : "s"}
                      </Badge>
                    )}
                  </div>
                </td>
                <td className="px-3 py-3">
                  <QuotaControl id={t.id} tokensPerMinute={t.tokens_per_minute} maxInFlight={t.max_in_flight} />
                </td>
                <td className="px-3 py-3">
                  <WeightControl id={t.id} initial={t.weight} />
                </td>
                <td className="px-3 py-3 text-right">
                  <Button
                    type="button"
                    size="icon"
                    variant="ghost"
                    aria-expanded={expanded}
                    title={expanded ? "Collapse" : "Edit tenant"}
                    onClick={() => setExpandedId((current) => (current === t.id ? null : t.id))}
                  >
                    <ChevronDown className={cn("h-3.5 w-3.5 transition-transform", expanded && "rotate-180")} />
                  </Button>
                </td>
              </tr>
              {expanded && (
                <tr className="border-b border-border/60">
                  <td colSpan={5} className="bg-background/35 px-6 py-5">
                    <TenantDetailPanel
                      tenant={t}
                      models={models}
                      pending={pending}
                      start={start}
                      onClose={() => setExpandedId(null)}
                    />
                  </td>
                </tr>
              )}
            </Fragment>
          );
        })}
        {tenants.length === 0 && (
          <tr>
            <td colSpan={5} className="px-6 py-8 text-center text-muted-foreground">
              No tenants yet.
            </td>
          </tr>
        )}
      </tbody>
    </table>
  );
}

function TenantDetailPanel({
  tenant,
  models,
  pending,
  start,
  onClose,
}: {
  tenant: Tenant;
  models: string[];
  pending: boolean;
  start: (cb: () => void) => void;
  onClose: () => void;
}) {
  function changeStatus(status: string) {
    start(() => setTenantStatusAction(tenant.id, status));
  }

  function remove() {
    if (
      !window.confirm(
        `Permanently delete tenant "${tenant.name}"? This removes all of its API keys and cannot be undone. Usage history is retained.`,
      )
    )
      return;
    start(() => deleteTenantAction(tenant.id));
  }

  return (
    <div className="space-y-5">
      <form
        action={(fd) =>
          start(async () => {
            await updateTenantAction(fd);
            onClose();
          })
        }
        className="grid gap-4 md:grid-cols-2"
      >
        <input type="hidden" name="id" value={tenant.id} />
        <div className="space-y-1.5">
          <Label htmlFor={`edit-name-${tenant.id}`}>Name</Label>
          <Input id={`edit-name-${tenant.id}`} name="name" required defaultValue={tenant.name} />
        </div>
        <div className="space-y-1.5">
          <Label htmlFor={`edit-org-${tenant.id}`}>Organization</Label>
          <Input
            id={`edit-org-${tenant.id}`}
            name="organization"
            defaultValue={tenant.organization}
            placeholder="Team, project, or customer"
          />
        </div>
        <div className="space-y-1.5 md:col-span-2">
          <Label htmlFor={`edit-desc-${tenant.id}`}>Description</Label>
          <Input
            id={`edit-desc-${tenant.id}`}
            name="description"
            defaultValue={tenant.description}
            placeholder="What this tenant is for"
          />
        </div>
        <div className="space-y-1.5">
          <Label htmlFor={`edit-contact-${tenant.id}`}>Contact email</Label>
          <Input
            id={`edit-contact-${tenant.id}`}
            name="contact_email"
            type="email"
            defaultValue={tenant.contact_email}
            placeholder="owner@example.com"
          />
        </div>
        <div className="flex items-end gap-2">
          <Button type="submit" disabled={pending}>
            {pending ? "Saving..." : "Save changes"}
          </Button>
        </div>
      </form>

      <ScheduleEditor tenant={tenant} />

      <BudgetEditor tenant={tenant} />

      <AllowlistEditor tenant={tenant} models={models} />

      <div className="flex flex-wrap items-center gap-2 border-t border-border/60 pt-4">
        <span className="text-xs text-muted-foreground">Lifecycle:</span>
        {tenant.status !== "active" && (
          <Button type="button" size="sm" variant="secondary" disabled={pending} onClick={() => changeStatus("active")}>
            Activate
          </Button>
        )}
        {tenant.status !== "suspended" && (
          <Button type="button" size="sm" variant="secondary" disabled={pending} onClick={() => changeStatus("suspended")}>
            Suspend
          </Button>
        )}
        {tenant.status !== "archived" && (
          <Button type="button" size="sm" variant="secondary" disabled={pending} onClick={() => changeStatus("archived")}>
            Archive
          </Button>
        )}
        <div className="ml-auto">
          <Button
            type="button"
            size="sm"
            variant="ghost"
            className="text-destructive hover:text-destructive"
            disabled={pending}
            onClick={remove}
          >
            <Trash2 className="h-3.5 w-3.5" />
            Delete tenant
          </Button>
        </div>
      </div>
    </div>
  );
}

/** ISO timestamp -> value for a <input type="datetime-local"> in browser-local time. */
function isoToLocalInput(iso: string | null): string {
  if (!iso) return "";
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "";
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/** datetime-local value -> ISO string (or null when empty). */
function localInputToIso(value: string): string | null {
  if (!value) return null;
  const d = new Date(value);
  return Number.isNaN(d.getTime()) ? null : d.toISOString();
}

/** minutes-from-midnight -> "HH:mm". */
function minToTime(min: number): string {
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${pad(Math.floor(min / 60))}:${pad(min % 60)}`;
}

/** "HH:mm" -> minutes-from-midnight. */
function timeToMin(value: string): number {
  const [h, m] = value.split(":").map((p) => parseInt(p, 10));
  return (h || 0) * 60 + (m || 0);
}

function ScheduleEditor({ tenant }: { tenant: Tenant }) {
  const [pending, start] = useTransition();
  const [timezone, setTimezone] = useState(tenant.timezone || "UTC");
  const [activeFrom, setActiveFrom] = useState(isoToLocalInput(tenant.active_from));
  const [activeUntil, setActiveUntil] = useState(isoToLocalInput(tenant.active_until));
  const [windows, setWindows] = useState<WeeklyWindow[]>(tenant.weekly_windows ?? []);
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);

  function addWindow() {
    setWindows((ws) => [...ws, { day: 1, start_min: 9 * 60, end_min: 17 * 60 }]);
  }
  function removeWindow(idx: number) {
    setWindows((ws) => ws.filter((_, i) => i !== idx));
  }
  function patchWindow(idx: number, patch: Partial<WeeklyWindow>) {
    setWindows((ws) => ws.map((w, i) => (i === idx ? { ...w, ...patch } : w)));
  }

  function save() {
    setError(null);
    setSaved(false);
    for (const w of windows) {
      if (w.end_min <= w.start_min) {
        setError("Each window's end time must be after its start time.");
        return;
      }
    }
    const fromIso = localInputToIso(activeFrom);
    const untilIso = localInputToIso(activeUntil);
    if (fromIso && untilIso && new Date(untilIso) <= new Date(fromIso)) {
      setError("Active-until must be after active-from.");
      return;
    }
    start(async () => {
      const res = await setTenantScheduleAction(tenant.id, {
        timezone: timezone.trim() || "UTC",
        active_from: fromIso,
        active_until: untilIso,
        weekly_windows: windows.length ? windows : null,
      });
      if (res.ok) {
        setSaved(true);
      } else {
        setError(res.error);
      }
    });
  }

  return (
    <div className="space-y-4 border-t border-border/60 pt-4">
      <div className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
        Access schedule
      </div>
      <div className="grid gap-4 md:grid-cols-3">
        <div className="space-y-1.5">
          <Label htmlFor={`tz-${tenant.id}`}>Timezone (IANA)</Label>
          <Input
            id={`tz-${tenant.id}`}
            value={timezone}
            onChange={(e) => setTimezone(e.target.value)}
            placeholder="UTC"
          />
        </div>
        <div className="space-y-1.5">
          <Label htmlFor={`from-${tenant.id}`}>Active from</Label>
          <Input
            id={`from-${tenant.id}`}
            type="datetime-local"
            value={activeFrom}
            onChange={(e) => setActiveFrom(e.target.value)}
          />
        </div>
        <div className="space-y-1.5">
          <Label htmlFor={`until-${tenant.id}`}>Active until</Label>
          <Input
            id={`until-${tenant.id}`}
            type="datetime-local"
            value={activeUntil}
            onChange={(e) => setActiveUntil(e.target.value)}
          />
        </div>
      </div>

      <div className="space-y-2">
        <div className="flex items-center justify-between">
          <span className="text-xs text-muted-foreground">
            Weekly windows (local to the tenant timezone). No windows = always on.
          </span>
          <Button type="button" size="sm" variant="secondary" onClick={addWindow}>
            <Plus className="h-3.5 w-3.5" />
            Add window
          </Button>
        </div>
        {windows.map((w, idx) => (
          <div key={idx} className="flex flex-wrap items-center gap-2">
            <select
              value={w.day}
              onChange={(e) => patchWindow(idx, { day: Number(e.target.value) })}
              className="h-9 rounded-md border border-input bg-background px-2 text-sm"
            >
              {DAY_LABELS.map((label, d) => (
                <option key={d} value={d}>
                  {label}
                </option>
              ))}
            </select>
            <Input
              type="time"
              className="w-32"
              value={minToTime(w.start_min)}
              onChange={(e) => patchWindow(idx, { start_min: timeToMin(e.target.value) })}
            />
            <span className="text-muted-foreground">to</span>
            <Input
              type="time"
              className="w-32"
              value={minToTime(w.end_min)}
              onChange={(e) => patchWindow(idx, { end_min: timeToMin(e.target.value) })}
            />
            <Button
              type="button"
              size="icon"
              variant="ghost"
              title="Remove window"
              onClick={() => removeWindow(idx)}
            >
              <X className="h-3.5 w-3.5" />
            </Button>
          </div>
        ))}
      </div>

      {error && <div className="text-sm text-destructive">{error}</div>}
      {saved && !error && <div className="text-sm text-emerald-500">Schedule saved.</div>}

      <div>
        <Button type="button" disabled={pending} onClick={save}>
          {pending ? "Saving..." : "Save schedule"}
        </Button>
      </div>
    </div>
  );
}

function BudgetEditor({ tenant }: { tenant: Tenant }) {
  const [pending, start] = useTransition();
  const [tokens, setTokens] = useState(
    tenant.budget_tokens != null ? String(tenant.budget_tokens) : "",
  );
  const [cost, setCost] = useState(
    tenant.budget_cost_usd != null ? String(tenant.budget_cost_usd) : "",
  );
  const [period, setPeriod] = useState(tenant.budget_period ?? "lifetime");
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);

  function save() {
    setError(null);
    setSaved(false);
    const tokensVal = tokens.trim() === "" ? null : Number(tokens);
    const costVal = cost.trim() === "" ? null : Number(cost);
    if (tokensVal != null && (!Number.isFinite(tokensVal) || tokensVal < 0)) {
      setError("Token cap must be a non-negative number.");
      return;
    }
    if (costVal != null && (!Number.isFinite(costVal) || costVal < 0)) {
      setError("Cost cap must be a non-negative number.");
      return;
    }
    start(async () => {
      const res = await setTenantBudgetAction(tenant.id, {
        budget_tokens: tokensVal,
        budget_cost_usd: costVal,
        budget_period: period,
      });
      if (res.ok) setSaved(true);
      else setError(res.error);
    });
  }

  function clearCaps() {
    setTokens("");
    setCost("");
    start(async () => {
      const res = await setTenantBudgetAction(tenant.id, {
        budget_tokens: null,
        budget_cost_usd: null,
        budget_period: period,
      });
      if (res.ok) setSaved(true);
      else setError(res.error);
    });
  }

  return (
    <div className="space-y-4 border-t border-border/60 pt-4">
      <div className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
        Cumulative budget caps
      </div>
      <div className="grid gap-4 md:grid-cols-3">
        <div className="space-y-1.5">
          <Label htmlFor={`budget-tokens-${tenant.id}`}>Token cap</Label>
          <Input
            id={`budget-tokens-${tenant.id}`}
            type="number"
            min={0}
            value={tokens}
            onChange={(e) => setTokens(e.target.value)}
            placeholder="unlimited"
          />
        </div>
        <div className="space-y-1.5">
          <Label htmlFor={`budget-cost-${tenant.id}`}>Cost cap (USD)</Label>
          <Input
            id={`budget-cost-${tenant.id}`}
            type="number"
            min={0}
            step="0.01"
            value={cost}
            onChange={(e) => setCost(e.target.value)}
            placeholder="unlimited"
          />
        </div>
        <div className="space-y-1.5">
          <Label htmlFor={`budget-period-${tenant.id}`}>Reset period</Label>
          <select
            id={`budget-period-${tenant.id}`}
            value={period}
            onChange={(e) => setPeriod(e.target.value)}
            className="h-9 w-full rounded-md border border-input bg-background px-2 text-sm"
          >
            {BUDGET_PERIODS.map((p) => (
              <option key={p} value={p}>
                {p}
              </option>
            ))}
          </select>
        </div>
      </div>
      <p className="text-xs text-muted-foreground">
        Caps apply to cumulative usage. <strong>monthly</strong> resets each calendar month (tenant
        timezone), <strong>term</strong> resets when re-applied, <strong>lifetime</strong> never
        resets. Leave a field blank for no cap.
      </p>

      {error && <div className="text-sm text-destructive">{error}</div>}
      {saved && !error && <div className="text-sm text-emerald-500">Budget saved.</div>}

      <div className="flex items-center gap-2">
        <Button type="button" disabled={pending} onClick={save}>
          {pending ? "Saving..." : "Save budget"}
        </Button>
        {(tenant.budget_tokens != null || tenant.budget_cost_usd != null) && (
          <Button type="button" variant="ghost" disabled={pending} onClick={clearCaps}>
            Clear caps
          </Button>
        )}
      </div>
    </div>
  );
}

function AllowlistEditor({ tenant, models }: { tenant: Tenant; models: string[] }) {
  const [pending, start] = useTransition();
  const [selected, setSelected] = useState<string[]>(tenant.allowed_models ?? []);
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);

  // Include any models that are on the allowlist but no longer registered.
  const options = Array.from(new Set([...models, ...selected]));

  function toggle(name: string) {
    setSaved(false);
    setSelected((cur) =>
      cur.includes(name) ? cur.filter((m) => m !== name) : [...cur, name],
    );
  }

  function save() {
    setError(null);
    setSaved(false);
    start(async () => {
      const res = await setTenantAllowlistAction(tenant.id, selected);
      if (res.ok) setSaved(true);
      else setError(res.error);
    });
  }

  return (
    <div className="space-y-3 border-t border-border/60 pt-4">
      <div className="text-xs font-medium uppercase tracking-wide text-muted-foreground">
        Model allowlist
      </div>
      <p className="text-xs text-muted-foreground">
        When empty, the tenant may use every registered model. Select models to restrict access.
      </p>
      {options.length === 0 ? (
        <p className="text-sm text-muted-foreground">No models registered yet.</p>
      ) : (
        <div className="flex flex-wrap gap-2">
          {options.map((name) => {
            const on = selected.includes(name);
            return (
              <button
                key={name}
                type="button"
                onClick={() => toggle(name)}
                className={cn(
                  "rounded-md border px-2.5 py-1 text-xs transition-colors",
                  on
                    ? "border-indigo-500/50 bg-indigo-500/10 text-indigo-500"
                    : "border-input text-muted-foreground hover:border-foreground/30",
                )}
              >
                {name}
              </button>
            );
          })}
        </div>
      )}

      {error && <div className="text-sm text-destructive">{error}</div>}
      {saved && !error && <div className="text-sm text-emerald-500">Allowlist saved.</div>}

      <div className="flex items-center gap-2">
        <Button type="button" disabled={pending} onClick={save}>
          {pending ? "Saving..." : "Save allowlist"}
        </Button>
        {selected.length > 0 && (
          <Button
            type="button"
            variant="ghost"
            disabled={pending}
            onClick={() => {
              setSelected([]);
              setSaved(false);
            }}
          >
            Clear ({selected.length})
          </Button>
        )}
      </div>
    </div>
  );
}

