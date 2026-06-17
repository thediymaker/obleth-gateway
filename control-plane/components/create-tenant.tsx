"use client";

import { useMemo, useRef, useState, useTransition, type ReactNode } from "react";
import { CalendarClock, Check, Info, Plus, RefreshCw, X } from "lucide-react";
import { createTenantAction } from "@/app/actions";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import type { WeeklyWindow } from "@/lib/obleth";
import { cn } from "@/lib/utils";

const DAY_LABELS = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const BUDGET_PERIODS = ["lifetime", "monthly", "term"] as const;
const STATUS_OPTIONS = ["active", "suspended", "archived"] as const;

export function CreateTenant({
  models,
  tenantWeights,
  onCreated,
  className,
}: {
  models: string[];
  tenantWeights: number[];
  onCreated?: () => void;
  className?: string;
}) {
  const formRef = useRef<HTMLFormElement>(null);
  const [pending, start] = useTransition();
  const [error, setError] = useState<string | null>(null);
  const [status, setStatus] = useState<(typeof STATUS_OPTIONS)[number]>("active");
  const [activeFrom, setActiveFrom] = useState("");
  const [activeUntil, setActiveUntil] = useState("");
  const [windows, setWindows] = useState<WeeklyWindow[]>([]);
  const [selectedModels, setSelectedModels] = useState<string[]>([]);
  const [weight, setWeight] = useState("");
  const weeklyWindowsJson = useMemo(() => JSON.stringify(windows), [windows]);
  const effectiveWeight = Math.max(1, Math.round(Number(weight) || 100));
  const existingWeightTotal = tenantWeights.reduce((sum, value) => sum + value, 0);

  function resetLocalState() {
    formRef.current?.reset();
    setStatus("active");
    setActiveFrom("");
    setActiveUntil("");
    setWindows([]);
    setSelectedModels([]);
    setWeight("");
    setError(null);
  }

  function addWindow() {
    setWindows((ws) => [...ws, { day: 1, start_min: 9 * 60, end_min: 17 * 60 }]);
  }

  function removeWindow(idx: number) {
    setWindows((ws) => ws.filter((_, i) => i !== idx));
  }

  function patchWindow(idx: number, patch: Partial<WeeklyWindow>) {
    setWindows((ws) => ws.map((w, i) => (i === idx ? { ...w, ...patch } : w)));
  }

  function toggleModel(name: string) {
    setSelectedModels((cur) =>
      cur.includes(name) ? cur.filter((model) => model !== name) : [...cur, name],
    );
  }

  return (
    <form
      ref={formRef}
      action={(fd) =>
        start(async () => {
          setError(null);
          const result = await createTenantAction(fd);
          if (result.ok) {
            resetLocalState();
            onCreated?.();
          } else {
            setError(result.error);
          }
        })
      }
      className={cn("flex min-h-0 flex-col", className)}
    >
      <input type="hidden" name="status" value={status} />
      <input type="hidden" name="active_from" value={localInputToIso(activeFrom) ?? ""} />
      <input type="hidden" name="active_until" value={localInputToIso(activeUntil) ?? ""} />
      <input type="hidden" name="weekly_windows" value={weeklyWindowsJson} />
      {selectedModels.map((model) => (
        <input key={model} type="hidden" name="allowed_models" value={model} />
      ))}

      <Tabs defaultValue="profile" className="flex min-h-0 flex-1 flex-col">
        <TabsList className="h-auto flex-wrap justify-start">
          <TabsTrigger value="profile">Profile</TabsTrigger>
          <TabsTrigger value="controls">Controls</TabsTrigger>
          <TabsTrigger value="schedule">Access</TabsTrigger>
          <TabsTrigger value="budget">Budgets</TabsTrigger>
          <TabsTrigger value="models">Models</TabsTrigger>
          <TabsTrigger value="lifecycle">Lifecycle</TabsTrigger>
        </TabsList>

        <TabsContent value="profile" className="min-h-0 flex-1 overflow-y-auto pr-1">
          <PanelCard title="Profile" description="Human-readable ownership details.">
            <div className="grid gap-x-4 gap-y-3 p-4 md:grid-cols-2">
              <Field label="Name" id="tenant-name" name="name" required placeholder="chatbot" />
              <Field
                label="Organization"
                id="tenant-organization"
                name="organization"
                placeholder="Team, project, or customer"
              />
              <div className="md:col-span-2">
                <Field
                  label="Description"
                  id="tenant-description"
                  name="description"
                  placeholder="What this tenant is for"
                />
              </div>
              <Field
                label="Contact email"
                id="tenant-contact-email"
                name="contact_email"
                type="email"
                placeholder="owner@example.com"
              />
            </div>
          </PanelCard>
        </TabsContent>

        <TabsContent value="controls" className="min-h-0 flex-1 overflow-y-auto pr-1">
          <PanelCard title="Traffic controls" description="Optional tuning knobs. Leave caps blank for unlimited.">
            <div className="grid gap-x-4 gap-y-3 p-4 md:grid-cols-2">
              <Field label="Fairshare group" id="tenant-group" name="fairshare_group" placeholder="default" />
              <div className="space-y-1.5">
                <div className="flex items-center gap-1.5">
                  <Label htmlFor="tenant-weight">Fairshare weight</Label>
                  <InfoTip>
                    Relative priority during contention. A tenant with weight 200 receives about twice the share of one with weight 100 in the same fairshare pool.
                  </InfoTip>
                </div>
                <Input
                  id="tenant-weight"
                  name="weight"
                  type="number"
                  min={1}
                  placeholder="Default 100"
                  value={weight}
                  onChange={(e) => setWeight(e.target.value)}
                />
              </div>
              <div className="space-y-1.5">
                <div className="flex items-center gap-1.5">
                  <Label htmlFor="tenant-tokens-per-minute">Tokens / min</Label>
                  <InfoTip>
                    Sustained token-rate cap for this tenant. Blank or 0 means unlimited at the tenant level; global and model limits can still apply.
                  </InfoTip>
                </div>
                <Input
                  id="tenant-tokens-per-minute"
                  name="tokens_per_minute"
                  type="number"
                  min={0}
                  placeholder="Unlimited"
                />
              </div>
              <div className="space-y-1.5">
                <div className="flex items-center gap-1.5">
                  <Label htmlFor="tenant-max-in-flight">Concurrency cap</Label>
                  <InfoTip>
                    Maximum requests for this tenant that may be actively running at the same time. Blank means unlimited at the tenant level; global and model capacity still apply.
                  </InfoTip>
                </div>
                <Input
                  id="tenant-max-in-flight"
                  name="max_in_flight"
                  type="number"
                  min={1}
                  placeholder="Unlimited"
                />
              </div>
              <div className="md:col-span-2">
                <WeightImpactMeter
                  weight={effectiveWeight}
                  peerWeightTotal={existingWeightTotal}
                  tenantCount={tenantWeights.length + 1}
                />
              </div>
            </div>
          </PanelCard>
        </TabsContent>

        <TabsContent value="schedule" className="min-h-0 flex-1 overflow-y-auto pr-1">
          <PanelCard title="Access schedule" description="Optional date range and weekly local windows.">
            <div className="space-y-4 p-4">
              <div className="grid gap-4 md:grid-cols-3">
                <Field label="Timezone (IANA)" id="tenant-timezone" name="timezone" defaultValue="UTC" placeholder="UTC" />
                <div className="space-y-1.5">
                  <Label htmlFor="tenant-active-from">Active from</Label>
                  <Input
                    id="tenant-active-from"
                    type="datetime-local"
                    value={activeFrom}
                    onChange={(e) => setActiveFrom(e.target.value)}
                  />
                </div>
                <div className="space-y-1.5">
                  <Label htmlFor="tenant-active-until">Active until</Label>
                  <Input
                    id="tenant-active-until"
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
                    No weekly windows means always on.
                  </span>
                  <Button type="button" size="sm" variant="secondary" onClick={addWindow}>
                    <Plus className="h-3.5 w-3.5" />
                    Add window
                  </Button>
                </div>
                <div className="space-y-2">
                  {windows.map((window, idx) => (
                    <WindowRow
                      key={idx}
                      window={window}
                      onPatch={(patch) => patchWindow(idx, patch)}
                      onRemove={() => removeWindow(idx)}
                    />
                  ))}
                </div>
              </div>
            </div>
          </PanelCard>
        </TabsContent>

        <TabsContent value="budget" className="min-h-0 flex-1 overflow-y-auto pr-1">
          <PanelCard title="Cumulative budget caps" description="Optional token or dollar ceilings.">
            <div className="grid gap-x-4 gap-y-3 p-4 md:grid-cols-3">
              <Field label="Token cap" id="tenant-budget-tokens" name="budget_tokens" type="number" min={0} placeholder="unlimited" />
              <Field
                label="Cost cap (USD)"
                id="tenant-budget-cost"
                name="budget_cost_usd"
                type="number"
                min={0}
                step="0.01"
                placeholder="unlimited"
              />
              <div className="space-y-1.5">
                <Label htmlFor="tenant-budget-period">Reset period</Label>
                <select
                  id="tenant-budget-period"
                  name="budget_period"
                  defaultValue="lifetime"
                  className="h-9 w-full rounded-md border border-input bg-background px-2 text-sm"
                >
                  {BUDGET_PERIODS.map((period) => (
                    <option key={period} value={period}>
                      {period}
                    </option>
                  ))}
                </select>
              </div>
            </div>
          </PanelCard>
        </TabsContent>

        <TabsContent value="models" className="min-h-0 flex-1 overflow-y-auto pr-1">
          <PanelCard title="Model allowlist" description="Leave empty to allow every registered model.">
            <div className="p-4">
              {models.length === 0 ? (
                <p className="text-sm text-muted-foreground">No models registered yet.</p>
              ) : (
                <div className="flex max-h-56 flex-wrap gap-2 overflow-auto pr-1">
                  {models.map((model) => {
                    const on = selectedModels.includes(model);
                    return (
                      <button
                        key={model}
                        type="button"
                        onClick={() => toggleModel(model)}
                        className={cn(
                          "inline-flex items-center gap-1.5 rounded-md border px-2.5 py-1 text-xs transition-colors",
                          on
                            ? "border-indigo-500/50 bg-indigo-500/10 text-indigo-500"
                            : "border-input text-muted-foreground hover:border-foreground/30 hover:text-foreground",
                        )}
                      >
                        {on && <Check className="h-3 w-3" strokeWidth={2.5} />}
                        {model}
                      </button>
                    );
                  })}
                </div>
              )}
            </div>
          </PanelCard>
        </TabsContent>

        <TabsContent value="lifecycle" className="min-h-0 flex-1 overflow-y-auto pr-1">
          <PanelCard title="Lifecycle" description="Initial tenant admission state.">
            <div className="flex flex-wrap gap-2 p-4">
              {STATUS_OPTIONS.map((option) => (
                <button
                  key={option}
                  type="button"
                  onClick={() => setStatus(option)}
                  className={cn(
                    "inline-flex items-center gap-1.5 rounded-md border px-3 py-1.5 text-xs font-medium transition-colors",
                    status === option
                      ? "border-primary/50 bg-primary/10 text-primary"
                      : "border-input text-muted-foreground hover:border-foreground/30 hover:text-foreground",
                  )}
                >
                  {status === option && <Check className="h-3 w-3" strokeWidth={2.5} />}
                  {option}
                </button>
              ))}
            </div>
          </PanelCard>
        </TabsContent>
      </Tabs>

      {error && (
        <div className="mt-3 rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive">
          {error}
        </div>
      )}

      <div className="mt-4 flex shrink-0 justify-end">
        <Button type="submit" disabled={pending}>
          {pending ? (
            <RefreshCw className="h-3.5 w-3.5 animate-spin" aria-hidden />
          ) : (
            <Plus className="h-3.5 w-3.5" aria-hidden />
          )}
          Create tenant
        </Button>
      </div>
    </form>
  );
}

function WindowRow({
  window,
  onPatch,
  onRemove,
}: {
  window: WeeklyWindow;
  onPatch: (patch: Partial<WeeklyWindow>) => void;
  onRemove: () => void;
}) {
  return (
    <div className="flex flex-wrap items-center gap-2 rounded-md border border-border/60 bg-background/30 p-2">
      <select
        value={window.day}
        onChange={(e) => onPatch({ day: Number(e.target.value) })}
        className="h-9 rounded-md border border-input bg-background px-2 text-sm"
      >
        {DAY_LABELS.map((label, day) => (
          <option key={day} value={day}>
            {label}
          </option>
        ))}
      </select>
      <Input
        type="time"
        className="w-32"
        value={minToTime(window.start_min)}
        onChange={(e) => onPatch({ start_min: timeToMin(e.target.value) })}
      />
      <span className="text-xs text-muted-foreground">to</span>
      <Input
        type="time"
        className="w-32"
        value={minToTime(window.end_min)}
        onChange={(e) => onPatch({ end_min: timeToMin(e.target.value) })}
      />
      <Button type="button" size="icon" variant="ghost" className="h-8 w-8" title="Remove window" onClick={onRemove}>
        <X className="h-3.5 w-3.5" />
      </Button>
    </div>
  );
}

function WeightImpactMeter({
  weight,
  peerWeightTotal,
  tenantCount,
}: {
  weight: number;
  peerWeightTotal: number;
  tenantCount: number;
}) {
  const total = Math.max(1, peerWeightTotal + weight);
  const share = weight / total;
  const average = tenantCount > 0 ? 1 / tenantCount : 1;
  const sharePct = Math.max(2, Math.min(100, share * 100));
  const averagePct = Math.max(0, Math.min(100, average * 100));

  return (
    <div className="rounded-md border border-border/60 bg-background/30 p-3">
      <div className="flex flex-wrap items-baseline justify-between gap-2">
        <div>
          <p className="text-xs font-medium">Estimated fairshare impact</p>
          <p className="mt-0.5 text-[11px] text-muted-foreground">
            Approximate capacity share while tenants are competing.
          </p>
        </div>
        <p className="text-sm font-semibold tabular-nums">{formatPercent(share)}</p>
      </div>
      <div className="relative mt-3 h-2 rounded-sm bg-muted">
        <div className="h-full rounded-sm bg-primary/70" style={{ width: `${sharePct}%` }} />
        <span
          aria-hidden
          className="absolute top-1/2 h-4 w-px -translate-y-1/2 bg-foreground/60"
          style={{ left: `${averagePct}%` }}
        />
      </div>
      <div className="mt-2 flex flex-wrap justify-between gap-2 text-[11px] text-muted-foreground">
        <span>Weight {weight}</span>
        <span>Average tenant share {formatPercent(average)}</span>
      </div>
    </div>
  );
}

function PanelCard({
  title,
  description,
  children,
}: {
  title: string;
  description?: string;
  children: ReactNode;
}) {
  return (
    <section className="overflow-hidden rounded-lg border border-border bg-card/40">
      <header className="border-b border-border/60 bg-background/30 px-4 py-2.5">
        <p className="text-sm font-medium">{title}</p>
        {description && <p className="text-xs text-muted-foreground">{description}</p>}
      </header>
      {children}
    </section>
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
  min,
  step,
}: {
  label: string;
  id: string;
  name: string;
  placeholder?: string;
  required?: boolean;
  type?: string;
  defaultValue?: string;
  min?: number;
  step?: string;
}) {
  return (
    <div className="space-y-1.5">
      <Label htmlFor={id}>{label}</Label>
      <Input
        id={id}
        name={name}
        type={type}
        placeholder={placeholder}
        required={required}
        defaultValue={defaultValue}
        min={min}
        step={step}
      />
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

function localInputToIso(value: string): string | null {
  if (!value) return null;
  const d = new Date(value);
  return Number.isNaN(d.getTime()) ? null : d.toISOString();
}

function minToTime(min: number): string {
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${pad(Math.floor(min / 60))}:${pad(min % 60)}`;
}

function timeToMin(value: string): number {
  const [h, m] = value.split(":").map((p) => parseInt(p, 10));
  return (h || 0) * 60 + (m || 0);
}

function formatPercent(value: number): string {
  if (!Number.isFinite(value)) return "0%";
  const pct = value * 100;
  return pct >= 10 ? `${pct.toFixed(0)}%` : `${pct.toFixed(1)}%`;
}
