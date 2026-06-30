"use client";

import {
  createContext,
  useContext,
  useState,
  useTransition,
  type ReactNode,
} from "react";
import {
  CalendarClock,
  Check,
  ChevronDown,
  Eye,
  Info,
  Plus,
  RefreshCw,
  Save,
  Shield,
  ShieldAlert,
  ShieldCheck,
  SlidersHorizontal,
  Trash2,
  X,
} from "lucide-react";
import type { LucideIcon } from "lucide-react";
import {
  deleteTenantAction,
  setTenantAllowlistAction,
  setTenantBudgetAction,
  setTenantCompressionAction,
  setTenantGuardrailsAction,
  setTenantScheduleAction,
  setTenantStatusAction,
  toggleTenantTracingAction,
  updateTenantAction,
} from "@/app/actions";
import { CreateTenant } from "@/components/create-tenant";
import { QuotaControl } from "@/components/quota-control";
import { WeightControl } from "@/components/weight-control";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import type { GuardrailsPolicy, Tenant, WeeklyWindow } from "@/lib/obleth";
import { cn, formatNumber } from "@/lib/utils";

const SaveFlashContext = createContext<() => void>(() => {});

const STATUS_STYLES: Record<string, string> = {
  active: "border-emerald-500/40 bg-emerald-500/10 text-emerald-500",
  suspended: "border-amber-500/40 bg-amber-500/10 text-amber-500",
  archived: "border-muted-foreground/40 bg-muted/30 text-muted-foreground",
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
  if (n >= 1_000_000_000) return `${(n / 1_000_000_000).toFixed(n % 1_000_000_000 === 0 ? 0 : 1)}B`;
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
    return { label: "Scheduled", className: "border-sky-500/40 bg-sky-500/10 text-sky-500" };
  }
  if (t.active_until && now >= new Date(t.active_until)) {
    return { label: "Expired", className: "border-muted-foreground/40 bg-muted/30 text-muted-foreground" };
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
      ? { label: "In window", className: "border-emerald-500/40 bg-emerald-500/10 text-emerald-500" }
      : { label: "Outside window", className: "border-amber-500/40 bg-amber-500/10 text-amber-500" };
  }
  return { label: "In window", className: "border-emerald-500/40 bg-emerald-500/10 text-emerald-500" };
}

export function TenantTable({ tenants, models }: { tenants: Tenant[]; models: string[] }) {
  const [expandedId, setExpandedId] = useState<string | null>(null);
  const [pending, start] = useTransition();
  const [createOpen, setCreateOpen] = useState(false);
  const [saveFlash, setSaveFlash] = useState<{ id: string; n: number } | null>(null);
  const flashSaved = (id: string) =>
    setSaveFlash((prev) => ({ id, n: prev?.id === id ? prev.n + 1 : 1 }));

  return (
    <div className="space-y-6">
      <TenantStats tenants={tenants} models={models} />

      <Dialog open={createOpen} onOpenChange={setCreateOpen}>
        <DialogContent className="grid h-[min(760px,85vh)] max-h-[85vh] max-w-4xl grid-rows-[auto_minmax(0,1fr)] overflow-hidden">
          <DialogHeader>
            <DialogTitle>Create tenant</DialogTitle>
            <DialogDescription>
              Add a fairshare unit with initial priority and optional safety caps.
            </DialogDescription>
          </DialogHeader>
          <CreateTenant
            models={models}
            tenantWeights={tenants.map((tenant) => tenant.weight)}
            onCreated={() => setCreateOpen(false)}
          />
        </DialogContent>
      </Dialog>

      <Card>
        <CardHeader className="gap-3 sm:flex-row sm:items-start sm:justify-between">
          <div>
            <CardTitle>Tenant configuration</CardTitle>
            <CardDescription>
              {formatNumber(tenants.length)} tenants / {formatNumber(activeTenantCount(tenants))} active
            </CardDescription>
          </div>
          <Button type="button" size="sm" onClick={() => setCreateOpen(true)}>
            <Plus className="h-3.5 w-3.5" />
            Add tenant
          </Button>
        </CardHeader>
        <CardContent className="p-0">
          <div className="text-sm">
            <div className="grid border-b border-border text-left text-xs text-muted-foreground md:grid-cols-[minmax(0,1.35fr)_minmax(10rem,0.45fr)_minmax(0,1fr)_auto]">
              <div className="px-6 py-3 font-medium">Tenant</div>
              <div className="hidden px-3 py-3 font-medium md:block">State</div>
              <div className="hidden px-3 py-3 font-medium md:block">
                <div className="flex items-center gap-1.5">
                  Configuration
                  <InfoTip>
                    Optional tuning fields. Token and concurrency caps show as unlimited unless a tenant-specific limit is set.
                  </InfoTip>
                </div>
              </div>
              <div className="hidden px-3 py-3 text-right font-medium md:block" />
            </div>

            <div className="space-y-3 px-4 py-4">
              {tenants.map((tenant) => {
                const expanded = expandedId === tenant.id;
                const subline = [tenant.organization, tenant.description].filter(Boolean).join(" - ");
                const schedule = scheduleBadge(tenant);

                return (
                  <div
                    key={tenant.id}
                    className={cn(
                      "group relative overflow-hidden rounded-lg border shadow-sm transition-colors",
                      expanded
                        ? "border-primary/35 bg-muted/25 ring-1 ring-primary/15"
                        : "border-border/70 bg-card/35 hover:border-border hover:bg-muted/15",
                    )}
                  >
                    {saveFlash?.id === tenant.id && (
                      <span
                        key={saveFlash.n}
                        aria-hidden
                        className="card-saved-glow pointer-events-none absolute inset-0 z-30 rounded-lg"
                      />
                    )}

                    <div className="grid min-w-0 md:grid-cols-[minmax(0,1.35fr)_minmax(10rem,0.45fr)_minmax(0,1fr)_auto] md:items-center">
                      <button
                        type="button"
                        onClick={() => setExpandedId((current) => (current === tenant.id ? null : tenant.id))}
                        className="min-w-0 px-5 py-3.5 pr-14 text-left md:pr-5"
                        aria-expanded={expanded}
                      >
                        <div className="flex min-w-0 items-center gap-2">
                          <p className="truncate font-medium" title={tenant.name}>
                            {tenant.name}
                          </p>
                        </div>
                        {subline && (
                          <p className="mt-0.5 line-clamp-2 text-xs leading-snug text-muted-foreground" title={subline}>
                            {subline}
                          </p>
                        )}
                        <div className="mt-1.5 flex flex-wrap items-center gap-1.5 md:hidden">
                          <TenantBadges tenant={tenant} schedule={schedule} />
                        </div>
                        <div className="mt-1.5 flex flex-wrap items-center gap-1.5 md:hidden">
                          <TenantConfigChips tenant={tenant} />
                        </div>
                      </button>

                      <div className="hidden min-w-0 px-3 py-3.5 md:block">
                        <div className="flex flex-wrap items-center gap-1.5">
                          <TenantBadges tenant={tenant} schedule={schedule} />
                        </div>
                      </div>

                      <div className="hidden min-w-0 px-3 py-3.5 md:block">
                        <div className="flex flex-wrap items-center gap-1.5">
                          <TenantConfigChips tenant={tenant} />
                        </div>
                      </div>

                      <div className="absolute right-3 top-3 md:static md:px-3 md:py-3.5">
                        <Button
                          type="button"
                          size="icon"
                          variant="ghost"
                          className="h-8 w-8 text-muted-foreground hover:text-foreground"
                          aria-expanded={expanded}
                          title={expanded ? "Collapse tenant" : "Edit tenant"}
                          onClick={() => setExpandedId((current) => (current === tenant.id ? null : tenant.id))}
                        >
                          <ChevronDown
                            className={cn(
                              "h-3.5 w-3.5 transition-transform duration-200",
                              expanded && "rotate-180 text-foreground",
                            )}
                          />
                        </Button>
                      </div>
                    </div>

                    {expanded && (
                      <div className="border-t border-border/60 bg-muted/10 px-5 py-4">
                        <SaveFlashContext.Provider value={() => flashSaved(tenant.id)}>
                          <TenantDetailPanel
                            tenant={tenant}
                            models={models}
                            peerWeightTotal={tenantWeightTotal(tenants, tenant.id)}
                            tenantCount={tenants.length}
                            pending={pending}
                            start={start}
                          />
                        </SaveFlashContext.Provider>
                      </div>
                    )}
                  </div>
                );
              })}

              {tenants.length === 0 && (
                <div className="rounded-lg border border-dashed border-border/70 px-6 py-10 text-center text-muted-foreground">
                  No tenants yet.
                </div>
              )}
            </div>
          </div>
        </CardContent>
      </Card>
    </div>
  );
}

function TenantBadges({ tenant, schedule }: { tenant: Tenant; schedule: ScheduleBadge | null }) {
  return (
    <>
      <Badge className={STATUS_STYLES[tenant.status] ?? STATUS_STYLES.archived}>{tenant.status}</Badge>
      {schedule && <Badge className={schedule.className}>{schedule.label}</Badge>}
      {(tenant.budget_tokens != null || tenant.budget_cost_usd != null) && (
        <Badge className="border-violet-500/40 bg-violet-500/10 text-violet-500">{budgetLabel(tenant)}</Badge>
      )}
      {tenant.allowed_models && tenant.allowed_models.length > 0 && (
        <Badge className="border-indigo-500/40 bg-indigo-500/10 text-indigo-500">
          {tenant.allowed_models.length} model{tenant.allowed_models.length === 1 ? "" : "s"}
        </Badge>
      )}
    </>
  );
}

function TenantConfigChips({ tenant }: { tenant: Tenant }) {
  return (
    <>
      <Badge className="border-border bg-background text-muted-foreground">
        weight {formatCompact(tenant.weight)}
      </Badge>
      <Badge className="border-border bg-background text-muted-foreground">
        {tenant.tokens_per_minute > 0 ? `${formatCompact(tenant.tokens_per_minute)} tok/min` : "tokens unlimited"}
      </Badge>
      <Badge className="border-border bg-background text-muted-foreground">
        {tenant.max_in_flight != null ? `${formatCompact(tenant.max_in_flight)} concurrent` : "concurrency unlimited"}
      </Badge>
    </>
  );
}

function TenantStats({ tenants, models }: { tenants: Tenant[]; models: string[] }) {
  const active = activeTenantCount(tenants);
  const scheduled = tenants.filter(
    (tenant) => tenant.active_from || tenant.active_until || (tenant.weekly_windows?.length ?? 0) > 0,
  ).length;
  const limited = tenants.filter(
    (tenant) => tenant.tokens_per_minute > 0 || tenant.max_in_flight != null,
  ).length;
  const restricted = tenants.filter((tenant) => (tenant.allowed_models?.length ?? 0) > 0).length;

  return (
    <div className="grid grid-cols-2 gap-3 lg:grid-cols-4">
      <StatCard label="Tenants" value={formatNumber(tenants.length)} hint={`${formatNumber(active)} active`} />
      <StatCard label="Scheduled access" value={formatNumber(scheduled)} hint="time-boxed tenants" />
      <StatCard label="Safety limits" value={formatNumber(limited)} hint="rate or concurrency caps" />
      <StatCard label="Model allowlists" value={formatNumber(restricted)} hint={`${formatNumber(models.length)} models available`} />
    </div>
  );
}

function activeTenantCount(tenants: Tenant[]) {
  return tenants.filter((tenant) => tenant.status === "active").length;
}

function tenantWeightTotal(tenants: Tenant[], excludedId: string) {
  return tenants.reduce((sum, tenant) => sum + (tenant.id === excludedId ? 0 : tenant.weight), 0);
}

function StatCard({ label, value, hint }: { label: string; value: string; hint?: string }) {
  return (
    <div className="rounded-lg border border-border bg-card p-4">
      <p className="text-xs text-muted-foreground">{label}</p>
      <p className="mt-1 text-2xl font-semibold tabular-nums">{value}</p>
      {hint && <p className="mt-0.5 text-[11px] text-muted-foreground">{hint}</p>}
    </div>
  );
}

function InfoTip({ children }: { children: string }) {
  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <button
          type="button"
          aria-label={children}
          className="inline-flex h-4 w-4 items-center justify-center rounded-sm text-muted-foreground hover:bg-muted/30 hover:text-foreground"
        >
          <Info className="h-3.5 w-3.5" />
        </button>
      </TooltipTrigger>
      <TooltipContent side="top" align="start" className="max-w-xs leading-relaxed">
        {children}
      </TooltipContent>
    </Tooltip>
  );
}

function TenantDetailPanel({
  tenant,
  models,
  peerWeightTotal,
  tenantCount,
  pending,
  start,
}: {
  tenant: Tenant;
  models: string[];
  peerWeightTotal: number;
  tenantCount: number;
  pending: boolean;
  start: (cb: () => void) => void;
}) {
  const flashSaved = useContext(SaveFlashContext);

  function changeStatus(status: string) {
    start(async () => {
      await setTenantStatusAction(tenant.id, status);
      flashSaved();
    });
  }

  function remove() {
    if (
      !window.confirm(
        `Permanently delete tenant "${tenant.name}"? This removes all of its API keys and cannot be undone. Usage history is retained.`,
      )
    ) {
      return;
    }
    start(() => deleteTenantAction(tenant.id));
  }

  return (
    <Tabs defaultValue="profile">
      <div className="flex flex-wrap items-center justify-between gap-3">
        <TabsList className="h-auto flex-wrap justify-start">
          <TabsTrigger value="profile">Profile</TabsTrigger>
          <TabsTrigger value="controls">Controls</TabsTrigger>
          <TabsTrigger value="schedule">Access</TabsTrigger>
          <TabsTrigger value="budget">Budgets</TabsTrigger>
          <TabsTrigger value="models">Models</TabsTrigger>
          <TabsTrigger value="guardrails">Guardrails</TabsTrigger>
          <TabsTrigger value="compression">Compression</TabsTrigger>
          <TabsTrigger value="lifecycle">Lifecycle</TabsTrigger>
        </TabsList>
        <p className="text-[11px] tabular-nums text-muted-foreground">
          Group {tenant.fairshare_group || "default"} / ID {tenant.id.slice(0, 8)}
        </p>
      </div>

      <TabsContent value="profile">
        <form
          action={(fd) =>
            start(async () => {
              await updateTenantAction(fd);
              flashSaved();
            })
          }
        >
          <input type="hidden" name="id" value={tenant.id} />
          <PanelCard
            title="Profile"
            description="Human-readable ownership details for this tenant."
            actions={<SaveButton pending={pending} idleLabel="Save profile" />}
          >
            <div className="grid gap-x-4 gap-y-3 p-4 md:grid-cols-2">
              <Field label="Name" id={`edit-name-${tenant.id}`} name="name" required defaultValue={tenant.name} />
              <Field
                label="Organization"
                id={`edit-org-${tenant.id}`}
                name="organization"
                defaultValue={tenant.organization}
                placeholder="Team, project, or customer"
              />
              <div className="md:col-span-2">
                <Field
                  label="Description"
                  id={`edit-desc-${tenant.id}`}
                  name="description"
                  defaultValue={tenant.description}
                  placeholder="What this tenant is for"
                />
              </div>
              <Field
                label="Contact email"
                id={`edit-contact-${tenant.id}`}
                name="contact_email"
                type="email"
                defaultValue={tenant.contact_email}
                placeholder="owner@example.com"
              />
              <div className="grid grid-cols-2 gap-3">
                <SpecItem label="Created">{formatDate(tenant.created_at)}</SpecItem>
                <SpecItem label="Updated">{formatDate(tenant.updated_at)}</SpecItem>
              </div>
            </div>
          </PanelCard>
        </form>
      </TabsContent>

      <TabsContent value="controls">
        <PanelCard
          title="Traffic controls"
          description="Live fairshare priority and per-tenant safety caps."
        >
          <div className="divide-y divide-border/60">
            <SettingRow
              label="Fairshare weight"
              hint="Relative priority when tenant demand exceeds shared capacity."
            >
              <div className="w-full max-w-md">
                <WeightControl
                  id={tenant.id}
                  initial={tenant.weight}
                  peerWeightTotal={peerWeightTotal}
                  tenantCount={tenantCount}
                  onSaved={flashSaved}
                />
              </div>
            </SettingRow>
            <SettingRow
              label="Safety limits"
              hint="Optional token-rate and concurrency caps. Clear either box and apply to make it unlimited."
            >
              <div className="w-full max-w-xl">
                <QuotaControl
                  id={tenant.id}
                  tokensPerMinute={tenant.tokens_per_minute}
                  maxInFlight={tenant.max_in_flight}
                  onSaved={flashSaved}
                />
              </div>
            </SettingRow>
            <SettingRow
              label="Request tracing"
              hint="Record per-hop span data for all requests made by this tenant's keys. Individual keys can also enable tracing independently."
            >
              <Button
                type="button"
                variant={tenant.tracing_enabled ? "default" : "outline"}
                size="sm"
                disabled={pending}
                className={tenant.tracing_enabled ? "border border-emerald-500/40 bg-emerald-950/40 text-emerald-400 hover:bg-emerald-950/60" : ""}
                onClick={() => start(() => toggleTenantTracingAction(tenant.id, !tenant.tracing_enabled))}
              >
                {tenant.tracing_enabled ? "⬡ Tracing on" : "Tracing off"}
              </Button>
            </SettingRow>
          </div>
        </PanelCard>
      </TabsContent>

      <TabsContent value="schedule">
        <ScheduleEditor tenant={tenant} />
      </TabsContent>

      <TabsContent value="budget">
        <BudgetEditor tenant={tenant} />
      </TabsContent>

      <TabsContent value="models">
        <AllowlistEditor tenant={tenant} models={models} />
      </TabsContent>

      <TabsContent value="guardrails">
        <GuardrailsEditor tenant={tenant} models={models} />
      </TabsContent>

      <TabsContent value="compression">
        <CompressionEditor tenant={tenant} />
      </TabsContent>

      <TabsContent value="lifecycle">
        <PanelCard
          title="Lifecycle"
          description="Change access state or permanently remove this tenant."
          actions={<Badge className={STATUS_STYLES[tenant.status] ?? STATUS_STYLES.archived}>{tenant.status}</Badge>}
        >
          <div className="space-y-4 p-4">
            <div className="flex flex-wrap items-center gap-2">
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
            </div>
            <div className="rounded-md border border-destructive/25 bg-destructive/5 p-3">
              <p className="text-xs font-medium text-destructive">Danger zone</p>
              <p className="mt-1 text-xs text-muted-foreground">
                Deleting a tenant also removes its API keys. Usage history is retained.
              </p>
              <Button
                type="button"
                size="sm"
                variant="ghost"
                className="mt-3 text-destructive hover:text-destructive"
                disabled={pending}
                onClick={remove}
              >
                <Trash2 className="h-3.5 w-3.5" />
                Delete tenant
              </Button>
            </div>
          </div>
        </PanelCard>
      </TabsContent>
    </Tabs>
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
  const flashSaved = useContext(SaveFlashContext);
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
        flashSaved();
      } else {
        setError(res.error);
      }
    });
  }

  return (
    <PanelCard
      title="Access schedule"
      description="Restrict tenant access by date range or local weekly windows."
      actions={<SaveButton type="button" pending={pending} onClick={save} idleLabel="Save schedule" />}
    >
      <div className="space-y-4 p-4">
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
          <div className="flex flex-wrap items-center justify-between gap-2">
            <span className="flex items-center gap-1.5 text-xs text-muted-foreground">
              <CalendarClock className="h-3.5 w-3.5" />
              Weekly windows are local to the tenant timezone. No windows means always on.
            </span>
            <Button type="button" size="sm" variant="secondary" onClick={addWindow}>
              <Plus className="h-3.5 w-3.5" />
              Add window
            </Button>
          </div>
          <div className="space-y-2">
            {windows.map((w, idx) => (
              <div
                key={idx}
                className="flex flex-wrap items-center gap-2 rounded-md border border-border/60 bg-background/30 p-2"
              >
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
                <span className="text-xs text-muted-foreground">to</span>
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
                  className="h-8 w-8"
                  title="Remove window"
                  onClick={() => removeWindow(idx)}
                >
                  <X className="h-3.5 w-3.5" />
                </Button>
              </div>
            ))}
          </div>
        </div>

        <StatusMessage error={error} saved={saved} savedText="Schedule saved." />
      </div>
    </PanelCard>
  );
}

function BudgetEditor({ tenant }: { tenant: Tenant }) {
  const flashSaved = useContext(SaveFlashContext);
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
      if (res.ok) {
        setSaved(true);
        flashSaved();
      } else {
        setError(res.error);
      }
    });
  }

  function clearCaps() {
    setTokens("");
    setCost("");
    setError(null);
    setSaved(false);
    start(async () => {
      const res = await setTenantBudgetAction(tenant.id, {
        budget_tokens: null,
        budget_cost_usd: null,
        budget_period: period,
      });
      if (res.ok) {
        setSaved(true);
        flashSaved();
      } else {
        setError(res.error);
      }
    });
  }

  return (
    <PanelCard
      title="Cumulative budget caps"
      description="Cap total token or dollar usage per tenant."
      actions={<SaveButton type="button" pending={pending} onClick={save} idleLabel="Save budget" />}
    >
      <div className="space-y-4 p-4">
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
          Caps apply to cumulative usage. Monthly resets each calendar month in the tenant timezone;
          term resets when re-applied; lifetime never resets. Leave a field blank for no cap.
        </p>
        <StatusMessage error={error} saved={saved} savedText="Budget saved." />
        {(tenant.budget_tokens != null || tenant.budget_cost_usd != null || tokens || cost) && (
          <Button type="button" variant="ghost" size="sm" disabled={pending} onClick={clearCaps}>
            Clear caps
          </Button>
        )}
      </div>
    </PanelCard>
  );
}

function AllowlistEditor({ tenant, models }: { tenant: Tenant; models: string[] }) {
  const flashSaved = useContext(SaveFlashContext);
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
      if (res.ok) {
        setSaved(true);
        flashSaved();
      } else {
        setError(res.error);
      }
    });
  }

  return (
    <PanelCard
      title="Model allowlist"
      description="Leave empty to allow every registered model."
      actions={<SaveButton type="button" pending={pending} onClick={save} idleLabel="Save allowlist" />}
    >
      <div className="space-y-4 p-4">
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
                    "inline-flex items-center gap-1.5 rounded-md border px-2.5 py-1 text-xs transition-colors",
                    on
                      ? "border-indigo-500/50 bg-indigo-500/10 text-indigo-500"
                      : "border-input text-muted-foreground hover:border-foreground/30 hover:text-foreground",
                  )}
                >
                  {on && <Check className="h-3 w-3" strokeWidth={2.5} />}
                  {name}
                </button>
              );
            })}
          </div>
        )}

        <StatusMessage error={error} saved={saved} savedText="Allowlist saved." />
        {selected.length > 0 && (
          <Button
            type="button"
            variant="ghost"
            size="sm"
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
    </PanelCard>
  );
}

type GuardrailsAction = GuardrailsPolicy["action"];

const GUARDRAIL_ACTIONS: { id: GuardrailsAction; label: string; hint: string }[] = [
  { id: "block", label: "Block", hint: "Reject flagged requests and responses with an error." },
  { id: "redact", label: "Redact", hint: "Replace matched personal info / keywords in place and continue." },
  { id: "log_only", label: "Log only", hint: "Record an alert but never block or modify traffic." },
];

const SCANNER_META: Record<string, { label: string; hint: string }> = {
  pii: { label: "Personal info (PII)", hint: "Detects SSNs, emails, phone numbers, and card numbers." },
  prompt_injection: {
    label: "Prompt injection",
    hint: "Detects jailbreak / instruction-override attempts. Always blocks regardless of action.",
  },
  ban_keywords: {
    label: "Banned keywords",
    hint: "Matches the keyword list below (case-insensitive, whole-word).",
  },
  harm: {
    label: "Harmful content (AI)",
    hint: "Classifies content with a registered guard model. Requires a guard model below.",
  },
};

const INPUT_SCANNERS = ["pii", "prompt_injection", "ban_keywords", "harm"] as const;
const OUTPUT_SCANNERS = ["pii", "ban_keywords", "harm"] as const;

type GuardrailsPreset = {
  id: string;
  label: string;
  tag: string;
  blurb: string;
  icon: LucideIcon;
  policy: GuardrailsPolicy;
};

const GUARDRAIL_PRESETS: GuardrailsPreset[] = [
  {
    id: "ferpa_pii",
    label: "FERPA / PII redaction",
    tag: "Redact PII both ways",
    blurb:
      "Redacts personal info (SSN, email, phone, card numbers) in requests and responses. Passes through on scanner error.",
    icon: ShieldCheck,
    policy: {
      action: "redact",
      input_scanners: ["pii"],
      output_scanners: ["pii"],
      guard_model: null,
      ban_keywords: [],
      fail_open: true,
    },
  },
  {
    id: "injection_defense",
    label: "Prompt-injection defense",
    tag: "Block injection attempts",
    blurb: "Blocks requests containing prompt-injection patterns before they reach the model or tool loop.",
    icon: ShieldAlert,
    policy: {
      action: "block",
      input_scanners: ["prompt_injection"],
      output_scanners: [],
      guard_model: null,
      ban_keywords: [],
      fail_open: true,
    },
  },
  {
    id: "monitor_only",
    label: "Monitor only",
    tag: "Log, never block",
    blurb: "Logs an alert when personal info appears, but never blocks or modifies traffic.",
    icon: Eye,
    policy: {
      action: "log_only",
      input_scanners: ["pii"],
      output_scanners: ["pii"],
      guard_model: null,
      ban_keywords: [],
      fail_open: true,
    },
  },
];

const CUSTOM_PRESET = {
  id: "custom",
  label: "Custom",
  tag: "Hand-pick everything",
  blurb: "Hand-pick the action, scanners, and keywords below.",
  icon: SlidersHorizontal,
};

/** Order-insensitive string-set equality. */
function sameSet(a: string[], b: string[]): boolean {
  if (a.length !== b.length) return false;
  const sb = new Set(b);
  return a.every((x) => sb.has(x));
}

function policiesEqual(a: GuardrailsPolicy, b: GuardrailsPolicy): boolean {
  return (
    a.action === b.action &&
    sameSet(a.input_scanners, b.input_scanners) &&
    sameSet(a.output_scanners, b.output_scanners) &&
    (a.guard_model || null) === (b.guard_model || null) &&
    sameSet(a.ban_keywords, b.ban_keywords) &&
    a.fail_open === b.fail_open
  );
}

/** Small iOS-style on/off switch. */
function ToggleSwitch({
  checked,
  onChange,
  disabled,
}: {
  checked: boolean;
  onChange: () => void;
  disabled?: boolean;
}) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      disabled={disabled}
      onClick={onChange}
      className={cn(
        "inline-flex h-6 w-11 shrink-0 items-center rounded-full border transition-colors",
        checked ? "border-primary/60 bg-primary/40" : "border-input bg-muted",
        disabled && "cursor-not-allowed opacity-50",
      )}
    >
      <span
        className={cn(
          "ml-0.5 h-5 w-5 rounded-full bg-foreground shadow transition-transform",
          checked && "translate-x-5",
        )}
      />
    </button>
  );
}

function ScannerToggle({
  scanner,
  on,
  onToggle,
}: {
  scanner: string;
  on: boolean;
  onToggle: () => void;
}) {
  const meta = SCANNER_META[scanner];
  return (
    <button
      type="button"
      onClick={onToggle}
      aria-pressed={on}
      className={cn(
        "flex w-full items-center gap-2.5 rounded-md border px-2.5 py-2 text-left text-sm transition-colors",
        on
          ? "border-primary/40 bg-primary/10 text-foreground"
          : "border-border/70 bg-background/35 text-muted-foreground hover:bg-accent",
      )}
    >
      <span
        className={cn(
          "flex h-4 w-4 shrink-0 items-center justify-center rounded-[4px] border",
          on ? "border-primary bg-primary text-primary-foreground" : "border-input",
        )}
      >
        {on && <Check className="h-3 w-3" strokeWidth={3} />}
      </span>
      <span className="flex-1">{meta.label}</span>
      <Tooltip>
        <TooltipTrigger asChild>
          <Info className="h-3.5 w-3.5 shrink-0 text-muted-foreground/70" />
        </TooltipTrigger>
        <TooltipContent className="max-w-xs">{meta.hint}</TooltipContent>
      </Tooltip>
    </button>
  );
}

/** Themed combobox for picking a guard model — replaces the native <select> popup. */
function GuardModelSelect({
  id,
  value,
  models,
  invalid,
  onChange,
}: {
  id: string;
  value: string;
  models: string[];
  invalid: boolean;
  onChange: (value: string) => void;
}) {
  const options = ["", ...models];
  const display = value || "— none —";
  return (
    <DropdownMenu>
      <DropdownMenuTrigger
        id={id}
        className={cn(
          "flex h-9 w-full items-center justify-between gap-2 rounded-md border bg-background px-3 text-sm transition-colors focus:outline-none focus:ring-1 focus:ring-ring data-[state=open]:ring-1 data-[state=open]:ring-ring",
          invalid ? "border-destructive" : "border-input",
        )}
      >
        <span className={cn("truncate", !value && "text-muted-foreground")}>{display}</span>
        <ChevronDown className="h-4 w-4 shrink-0 text-muted-foreground" />
      </DropdownMenuTrigger>
      <DropdownMenuContent
        align="start"
        className="max-h-72 w-[var(--radix-dropdown-menu-trigger-width)] overflow-y-auto"
      >
        {options.map((m) => {
          const selected = m === value;
          return (
            <DropdownMenuItem
              key={m || "__none__"}
              onSelect={() => onChange(m)}
              className="cursor-pointer justify-between gap-2"
            >
              <span className={cn("truncate", !m && "text-muted-foreground")}>{m || "— none —"}</span>
              {selected && <Check className="h-3.5 w-3.5 shrink-0 text-primary" strokeWidth={2.5} />}
            </DropdownMenuItem>
          );
        })}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

function GuardrailsEditor({ tenant, models }: { tenant: Tenant; models: string[] }) {
  const flashSaved = useContext(SaveFlashContext);
  const [pending, start] = useTransition();
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);

  const initial = tenant.guardrails_policy;
  const [enabled, setEnabled] = useState(initial != null);
  const [action, setAction] = useState<GuardrailsAction>(initial?.action ?? "redact");
  const [inputScanners, setInputScanners] = useState<string[]>(initial?.input_scanners ?? ["pii"]);
  const [outputScanners, setOutputScanners] = useState<string[]>(initial?.output_scanners ?? ["pii"]);
  const [guardModel, setGuardModel] = useState(initial?.guard_model ?? "");
  const [banKeywords, setBanKeywords] = useState((initial?.ban_keywords ?? []).join("\n"));
  const [failOpen, setFailOpen] = useState(initial?.fail_open ?? true);

  const parsedKeywords = banKeywords
    .split("\n")
    .map((k) => k.trim())
    .filter(Boolean);

  const currentPolicy: GuardrailsPolicy = {
    action,
    input_scanners: inputScanners,
    output_scanners: outputScanners,
    guard_model: guardModel.trim() || null,
    ban_keywords: parsedKeywords,
    fail_open: failOpen,
  };

  const activePreset = GUARDRAIL_PRESETS.find((p) => policiesEqual(p.policy, currentPolicy))?.id ?? "custom";
  const allProfiles = [...GUARDRAIL_PRESETS, CUSTOM_PRESET];
  const activeProfile = allProfiles.find((p) => p.id === activePreset) ?? CUSTOM_PRESET;

  // Customize opens automatically when the loaded policy doesn't match a preset.
  const [customizeOpen, setCustomizeOpen] = useState(initial != null && activePreset === "custom");

  const harmSelected = inputScanners.includes("harm") || outputScanners.includes("harm");
  const usesKeywords = inputScanners.includes("ban_keywords") || outputScanners.includes("ban_keywords");
  const missingGuardModel = harmSelected && !guardModel.trim();
  const noScanners = inputScanners.length === 0 && outputScanners.length === 0;

  function dirty() {
    setSaved(false);
  }

  function applyPreset(p: GuardrailsPreset) {
    setAction(p.policy.action);
    setInputScanners(p.policy.input_scanners);
    setOutputScanners(p.policy.output_scanners);
    setGuardModel(p.policy.guard_model ?? "");
    setBanKeywords((p.policy.ban_keywords ?? []).join("\n"));
    setFailOpen(p.policy.fail_open);
    dirty();
  }

  function toggleScanner(list: string[], set: (v: string[]) => void, name: string) {
    dirty();
    set(list.includes(name) ? list.filter((s) => s !== name) : [...list, name]);
  }

  function summary(): string {
    if (!enabled) return "Guardrails are off for this tenant.";
    if (noScanners) return "No scanners selected — this policy does nothing.";
    const where =
      inputScanners.length && outputScanners.length
        ? "requests and responses"
        : inputScanners.length
          ? "requests"
          : "responses";
    const verb =
      action === "block" ? "Blocks flagged" : action === "redact" ? "Redacts matches in" : "Monitors";
    const failClause = failOpen ? "Passes through on scanner error." : "Returns 503 on scanner error.";
    return `${verb} ${where}. ${failClause}`;
  }

  function save() {
    setError(null);
    setSaved(false);
    if (enabled && missingGuardModel) {
      setError("Select a guard model — the Harmful content scanner needs one.");
      return;
    }
    const policy: GuardrailsPolicy | null = enabled ? currentPolicy : null;
    start(async () => {
      const res = await setTenantGuardrailsAction(tenant.id, policy);
      if (res.ok) {
        setSaved(true);
        flashSaved();
      } else {
        setError(res.error);
      }
    });
  }

  return (
    <PanelCard
      title="Content guardrails"
      description="Screen requests and responses with in-process scanners. The policy is enforced for every key in this tenant."
      actions={<SaveButton type="button" pending={pending} disabled={enabled && missingGuardModel} onClick={save} idleLabel="Save guardrails" />}
    >
      <div className="space-y-5 p-4">
        {/* enable strip */}
        <div
          className={cn(
            "flex items-center gap-3 rounded-lg border px-4 py-3 transition-colors",
            enabled ? "border-primary/40 bg-primary/5" : "border-border/70 bg-background/35",
          )}
        >
          <Shield className={cn("h-5 w-5 shrink-0", enabled ? "text-primary" : "text-muted-foreground")} />
          <div className="min-w-0 flex-1">
            <p className="text-sm font-medium">{enabled ? "Scanning enabled" : "Scanning off"}</p>
            <p className="text-xs leading-snug text-muted-foreground">{summary()}</p>
          </div>
          <ToggleSwitch checked={enabled} disabled={pending} onChange={() => { setEnabled((v) => !v); dirty(); }} />
        </div>

        {enabled && (
          <>
            {/* profile cards */}
            <div>
              <p className="mb-2 text-[10px] font-medium uppercase tracking-wider text-muted-foreground">Profile</p>
              <div className="grid gap-2.5 sm:grid-cols-2">
                {allProfiles.map((p) => {
                  const selected = activePreset === p.id;
                  const Icon = p.icon;
                  return (
                    <button
                      key={p.id}
                      type="button"
                      aria-pressed={selected}
                      onClick={() => (p.id === "custom" ? setCustomizeOpen(true) : applyPreset(p as GuardrailsPreset))}
                      className={cn(
                        "flex items-start gap-3 rounded-lg border p-3 text-left transition-colors",
                        selected
                          ? "border-primary/50 bg-primary/10"
                          : "border-border/70 bg-background/35 hover:bg-accent",
                      )}
                    >
                      <Icon className={cn("mt-0.5 h-5 w-5 shrink-0", selected ? "text-primary" : "text-muted-foreground")} />
                      <span className="min-w-0 flex-1">
                        <span className="flex items-center gap-2">
                          <span className="text-sm font-medium leading-tight">{p.label}</span>
                          {selected && <Check className="h-3.5 w-3.5 shrink-0 text-primary" strokeWidth={2.5} />}
                        </span>
                        <span className="mt-0.5 block text-[11px] leading-snug text-muted-foreground">{p.tag}</span>
                      </span>
                    </button>
                  );
                })}
              </div>
              <p className="mt-2.5 text-xs leading-relaxed text-muted-foreground">{activeProfile.blurb}</p>
            </div>

            {/* customize */}
            <div className="overflow-hidden rounded-lg border border-border/70">
              <button
                type="button"
                onClick={() => setCustomizeOpen((v) => !v)}
                className="flex w-full items-center justify-between bg-background/35 px-3 py-2.5 text-left text-sm font-medium hover:bg-accent"
              >
                <span className="flex items-center gap-2">
                  <SlidersHorizontal className="h-4 w-4 text-muted-foreground" />
                  Customize
                  {activePreset === "custom" && (
                    <Badge className="bg-primary/15 text-[10px] text-primary">Custom</Badge>
                  )}
                </span>
                <ChevronDown className={cn("h-4 w-4 transition-transform", customizeOpen && "rotate-180")} />
              </button>

              {customizeOpen && (
                <div className="space-y-5 border-t border-border/70 p-4">
                  {/* action */}
                  <div className="space-y-1.5">
                    <p className="text-[10px] font-medium uppercase tracking-wider text-muted-foreground">Action</p>
                    <div className="inline-flex rounded-md border border-input bg-background/50 p-0.5">
                      {GUARDRAIL_ACTIONS.map((a) => (
                        <Tooltip key={a.id}>
                          <TooltipTrigger asChild>
                            <button
                              type="button"
                              onClick={() => { setAction(a.id); dirty(); }}
                              className={cn(
                                "rounded px-3 py-1 text-xs font-medium transition-colors",
                                action === a.id
                                  ? "bg-primary text-primary-foreground"
                                  : "text-muted-foreground hover:text-foreground",
                              )}
                            >
                              {a.label}
                            </button>
                          </TooltipTrigger>
                          <TooltipContent className="max-w-xs">{a.hint}</TooltipContent>
                        </Tooltip>
                      ))}
                    </div>
                  </div>

                  {/* scanners */}
                  <div className="grid gap-5 sm:grid-cols-2">
                    <div className="space-y-1.5">
                      <p className="text-[10px] font-medium uppercase tracking-wider text-muted-foreground">Scan requests for</p>
                      <div className="space-y-1.5">
                        {INPUT_SCANNERS.map((s) => (
                          <ScannerToggle
                            key={s}
                            scanner={s}
                            on={inputScanners.includes(s)}
                            onToggle={() => toggleScanner(inputScanners, setInputScanners, s)}
                          />
                        ))}
                      </div>
                    </div>
                    <div className="space-y-1.5">
                      <p className="text-[10px] font-medium uppercase tracking-wider text-muted-foreground">Scan responses for</p>
                      <div className="space-y-1.5">
                        {OUTPUT_SCANNERS.map((s) => (
                          <ScannerToggle
                            key={s}
                            scanner={s}
                            on={outputScanners.includes(s)}
                            onToggle={() => toggleScanner(outputScanners, setOutputScanners, s)}
                          />
                        ))}
                      </div>
                    </div>
                  </div>

                  {/* conditional config: keywords + guard model */}
                  {(usesKeywords || harmSelected) && (
                    <div className="grid gap-5 sm:grid-cols-2">
                      {usesKeywords && (
                        <div className="space-y-1.5">
                          <Label htmlFor={`guardrails-keywords-${tenant.id}`} className="text-[10px] font-medium uppercase tracking-wider text-muted-foreground">
                            Banned keywords
                          </Label>
                          <textarea
                            id={`guardrails-keywords-${tenant.id}`}
                            value={banKeywords}
                            onChange={(e) => { setBanKeywords(e.target.value); dirty(); }}
                            rows={4}
                            placeholder={"confidential\nrestricted\n..."}
                            className="block w-full resize-y rounded-md border border-input bg-background px-3 py-2 text-sm placeholder:text-muted-foreground focus:outline-none focus:ring-1 focus:ring-ring"
                          />
                          <p className="text-[11px] leading-snug text-muted-foreground">
                            One per line. Case-insensitive, whole-word matching.
                          </p>
                        </div>
                      )}

                      {harmSelected && (
                        <div className="space-y-1.5">
                          <Label htmlFor={`guardrails-model-${tenant.id}`} className="text-[10px] font-medium uppercase tracking-wider text-muted-foreground">
                            Guard model {missingGuardModel && <span className="normal-case text-destructive">— required</span>}
                          </Label>
                          {models.length > 0 ? (
                            <GuardModelSelect
                              id={`guardrails-model-${tenant.id}`}
                              value={guardModel}
                              models={models}
                              invalid={missingGuardModel}
                              onChange={(v) => { setGuardModel(v); dirty(); }}
                            />
                          ) : (
                            <Input
                              id={`guardrails-model-${tenant.id}`}
                              value={guardModel}
                              onChange={(e) => { setGuardModel(e.target.value); dirty(); }}
                              placeholder="model name"
                              className={cn(missingGuardModel && "border-destructive")}
                            />
                          )}
                          <p className="text-[11px] leading-snug text-muted-foreground">
                            Classifies harmful content (e.g. Llama Guard, ShieldGemma).
                          </p>
                        </div>
                      )}
                    </div>
                  )}

                  {/* fail mode */}
                  <div className="flex items-center justify-between gap-4 border-t border-border/60 pt-4">
                    <div className="min-w-0">
                      <p className="text-sm font-medium">Fail open</p>
                      <p className="text-[11px] leading-snug text-muted-foreground">
                        On: a scanner error (guard model down, timeout) lets the request through unchanged.
                        Off: return 503 on scanner failure.
                      </p>
                    </div>
                    <ToggleSwitch checked={failOpen} disabled={pending} onChange={() => { setFailOpen((v) => !v); dirty(); }} />
                  </div>

                  <p className="text-[11px] leading-snug text-muted-foreground">
                    Response scanning buffers the reply (non-streaming) when the action is Block or Redact.
                  </p>
                </div>
              )}
            </div>
          </>
        )}

        <StatusMessage error={error} saved={saved} savedText="Guardrails saved." />
      </div>
    </PanelCard>
  );
}

function CompressionEditor({ tenant }: { tenant: Tenant }) {
  const flashSaved = useContext(SaveFlashContext);
  const [pending, start] = useTransition();
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);

  const policy = tenant.compression_policy;
  const [enabled, setEnabled] = useState(policy?.enabled ?? false);
  const [codeCompaction, setCodeCompaction] = useState(policy?.code_compaction ?? false);
  const [dedup, setDedup] = useState(policy?.dedup ?? false);
  const [compactLogs, setCompactLogs] = useState(policy?.compact_logs ?? false);
  const [allowLossy, setAllowLossy] = useState(policy?.allow_lossy ?? false);

  function save() {
    setError(null);
    setSaved(false);
    start(async () => {
      const res = await setTenantCompressionAction(tenant.id, {
        enabled,
        code_compaction: codeCompaction,
        dedup,
        compact_logs: compactLogs,
        allow_lossy: allowLossy,
      });
      if (res.ok) {
        setSaved(true);
        flashSaved();
      } else {
        setError(res.error);
      }
    });
  }

  function clearPolicy() {
    setError(null);
    setSaved(false);
    start(async () => {
      const res = await setTenantCompressionAction(tenant.id, null);
      if (res.ok) {
        setSaved(true);
        flashSaved();
      } else {
        setError(res.error);
      }
    });
  }

  return (
    <PanelCard
      title="Compression"
      description="Control context-compression pieces applied to this tenant's requests."
      actions={<SaveButton type="button" pending={pending} onClick={save} idleLabel="Save compression" />}
    >
      <div className="divide-y divide-border/60">
        <div className="flex items-center justify-between gap-4 px-4 py-3">
          <div className="min-w-0">
            <p className="text-sm font-medium">Enabled</p>
            <p className="text-[11px] leading-snug text-muted-foreground">
              Master switch for compression on this tenant.
            </p>
          </div>
          <ToggleSwitch checked={enabled} disabled={pending} onChange={() => setEnabled((v) => !v)} />
        </div>
        <div className="flex items-center justify-between gap-4 px-4 py-3">
          <div className="min-w-0">
            <p className="text-sm font-medium">Code compaction</p>
            <p className="text-[11px] leading-snug text-muted-foreground">
              Conservative whitespace stripping of fenced code.
            </p>
          </div>
          <ToggleSwitch checked={codeCompaction} disabled={pending} onChange={() => setCodeCompaction((v) => !v)} />
        </div>
        <div className="flex items-center justify-between gap-4 px-4 py-3">
          <div className="min-w-0">
            <p className="text-sm font-medium">Cross-turn dedup</p>
            <p className="text-[11px] leading-snug text-muted-foreground">
              Replace repeated blocks with a reference (reversible).
            </p>
          </div>
          <ToggleSwitch checked={dedup} disabled={pending} onChange={() => setDedup((v) => !v)} />
        </div>
        <div className="flex items-center justify-between gap-4 px-4 py-3">
          <div className="min-w-0">
            <p className="text-sm font-medium">Log compaction</p>
            <p className="text-[11px] leading-snug text-muted-foreground">
              Collapse repeated log lines, keep errors/warns (reversible).
            </p>
          </div>
          <ToggleSwitch checked={compactLogs} disabled={pending} onChange={() => setCompactLogs((v) => !v)} />
        </div>
        <div className="flex items-center justify-between gap-4 px-4 py-3">
          <div className="min-w-0">
            <p className="text-sm font-medium">Allow lossy</p>
            <p className="text-[11px] leading-snug text-muted-foreground">
              Summarize long prose with a helper model (reversible).
            </p>
          </div>
          <ToggleSwitch checked={allowLossy} disabled={pending} onChange={() => setAllowLossy((v) => !v)} />
        </div>
        <div className="space-y-3 px-4 py-3">
          <p className="text-[11px] leading-snug text-muted-foreground">
            No policy = follow the global default (lossless JSON on; dedup/lossy off). Lossy and dedup also require
            the model to support function calling and the tool loop to be enabled.
          </p>
          <Button type="button" size="sm" variant="secondary" disabled={pending} onClick={clearPolicy}>
            Clear policy
          </Button>
          <StatusMessage error={error} saved={saved} savedText="Compression policy saved." />
        </div>
      </div>
    </PanelCard>
  );
}

function SaveButton({
  pending,
  idleLabel = "Save",
  size = "sm",
  variant = "default",
  disabled,
  type = "submit",
  onClick,
}: {
  pending: boolean;
  idleLabel?: string;
  size?: "sm" | "default";
  variant?: "default" | "secondary";
  disabled?: boolean;
  type?: "submit" | "button";
  onClick?: () => void;
}) {
  return (
    <Button type={type} onClick={onClick} size={size} variant={variant} disabled={pending || disabled} aria-busy={pending}>
      {pending ? (
        <RefreshCw className="h-3.5 w-3.5 animate-spin" aria-hidden />
      ) : (
        <Save className="h-3.5 w-3.5" aria-hidden />
      )}
      {idleLabel}
    </Button>
  );
}

function PanelCard({
  title,
  description,
  actions,
  className,
  children,
}: {
  title?: string;
  description?: string;
  actions?: ReactNode;
  className?: string;
  children: ReactNode;
}) {
  return (
    <section className={cn("overflow-hidden rounded-lg border border-border bg-card/40", className)}>
      {title && (
        <header className="flex items-center justify-between gap-3 border-b border-border/60 bg-background/30 px-4 py-2.5">
          <div className="min-w-0 flex-1">
            <p className="text-sm font-medium">{title}</p>
            {description && <p className="text-xs text-muted-foreground">{description}</p>}
          </div>
          {actions && <div className="flex shrink-0 items-center gap-2">{actions}</div>}
        </header>
      )}
      {children}
    </section>
  );
}

function SettingRow({ label, hint, children }: { label: string; hint?: string; children: ReactNode }) {
  return (
    <div className="flex flex-col gap-3 px-4 py-3 lg:flex-row lg:items-center lg:justify-between">
      <div className="min-w-0 max-w-sm">
        <p className="text-xs font-medium">{label}</p>
        {hint && <p className="mt-0.5 text-[11px] leading-snug text-muted-foreground">{hint}</p>}
      </div>
      <div className="min-w-0 flex-1 lg:flex lg:justify-end">{children}</div>
    </div>
  );
}

function Field({
  label,
  id,
  name,
  placeholder,
  required,
  type = "text",
  defaultValue,
}: {
  label: string;
  id: string;
  name: string;
  placeholder?: string;
  required?: boolean;
  type?: string;
  defaultValue?: string;
}) {
  return (
    <div className="space-y-1.5">
      <Label htmlFor={id}>{label}</Label>
      <Input id={id} name={name} type={type} placeholder={placeholder} required={required} defaultValue={defaultValue} />
    </div>
  );
}

function SpecItem({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="min-w-0 rounded-md border border-border/60 bg-background/30 px-3 py-2">
      <p className="text-[10px] uppercase tracking-wider text-muted-foreground">{label}</p>
      <div className="mt-1 truncate text-xs font-medium tabular-nums">{children}</div>
    </div>
  );
}

function StatusMessage({
  error,
  saved,
  savedText,
}: {
  error: string | null;
  saved: boolean;
  savedText: string;
}) {
  if (error) return <div className="text-sm text-destructive">{error}</div>;
  if (saved) return <div className="text-sm text-emerald-500">{savedText}</div>;
  return null;
}

function formatDate(value: string) {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return "-";
  return date.toLocaleDateString([], { month: "short", day: "numeric", year: "numeric" });
}
