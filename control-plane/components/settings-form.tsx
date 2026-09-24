"use client";

import { useState, useTransition, type ReactNode } from "react";
import { useRouter } from "next/navigation";
import {
  Archive,
  BookOpen,
  Braces,
  ChevronDown,
  Database,
  Eye,
  Image as ImageIcon,
  Plus,
  Rabbit,
  RefreshCw,
  Save,
  Send,
  Server,
  Sparkles,
  Trash2,
  Wrench,
} from "lucide-react";
import type { LucideIcon } from "lucide-react";
import {
  setAlertSettingsAction,
  setAutoRouterSettingsAction,
  setBoonSettingsAction,
  setCharoSettingsAction,
  setKnowledgeSettingsAction,
  testAlertAction,
  setSlurmSettingsAction,
  testSlurmConnectionAction,
  setUsageRetentionAction,
  compactUsageAction,
  resyncCacheAction,
} from "@/app/actions";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Checkbox } from "@/components/ui/checkbox";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Select } from "@/components/ui/select";
import { DestructiveConfirm } from "@/components/ui/destructive-confirm";
import { cn, tagsInclude } from "@/lib/utils";
import { formatBytes } from "@/lib/format";
import type {
  AlertSettingsView,
  AutoRouterSettingsView,
  BoonSettingsView,
  CharoSettingsView,
  CompressorStatusView,
  KnowledgeSettingsView,
  ModelRoute,
  NodeAlias,
  SlurmHealthView,
  SlurmSettingsView,
  SpeculationCategoryGate,
  UpdateAlertSettings,
  UpdateAutoRouterSettings,
  UpdateBoonSettings,
  UpdateKnowledgeSettings,
  UpdateSlurmSettings,
  UsageRetentionView,
  ResyncReport,
} from "@/lib/obleth";

const RETENTION_PRESETS = [7, 30, 90, 180, 365] as const;

type ChannelResult = { channel: string; ok: boolean; detail: string };

export function AlertSettingsForm({ settings }: { settings: AlertSettingsView | null }) {
  const [pending, start] = useTransition();
  const [testing, startTest] = useTransition();
  const [status, setStatus] = useState<{ ok: boolean; message: string } | null>(null);
  const [testResults, setTestResults] = useState<ChannelResult[] | null>(null);

  // Slack
  const slackSet = settings?.slack_webhook_set ?? false;
  const [slackWebhook, setSlackWebhook] = useState("");
  const [clearSlack, setClearSlack] = useState(false);
  const [minInterval, setMinInterval] = useState(String(settings?.min_interval_secs ?? 300));

  // Email
  const email = settings?.email ?? null;
  const [emailEnabled, setEmailEnabled] = useState(Boolean(email));
  const [smtpHost, setSmtpHost] = useState(email?.smtp_host ?? "");
  const [smtpPort, setSmtpPort] = useState(String(email?.smtp_port ?? 587));
  const [username, setUsername] = useState(email?.username ?? "");
  const passwordSet = email?.password_set ?? false;
  const [password, setPassword] = useState("");
  const [clearPassword, setClearPassword] = useState(false);
  const [fromAddress, setFromAddress] = useState(email?.from_address ?? "");
  const [recipients, setRecipients] = useState((email?.recipients ?? []).join(", "));
  const [starttls, setStarttls] = useState(email?.starttls ?? true);

  function buildBody(): UpdateAlertSettings {
    const body: UpdateAlertSettings = {
      min_interval_secs: Number(minInterval) || 0,
    };
    if (slackWebhook.trim()) {
      body.slack_webhook_url = slackWebhook.trim();
    } else if (clearSlack) {
      body.clear_slack_webhook = true;
    }
    if (emailEnabled) {
      body.email = {
        smtp_host: smtpHost.trim(),
        smtp_port: Number(smtpPort) || 587,
        username: username.trim() || null,
        from_address: fromAddress.trim(),
        recipients: recipients
          .split(/[\n,]/)
          .map((r) => r.trim())
          .filter(Boolean),
        starttls,
      };
      if (password.trim()) {
        body.email.smtp_password = password.trim();
      } else if (clearPassword) {
        body.email.clear_smtp_password = true;
      }
    } else {
      body.email = null;
    }
    return body;
  }

  function save() {
    setStatus(null);
    setTestResults(null);
    start(async () => {
      const result = await setAlertSettingsAction(buildBody());
      if (result.ok) {
        setStatus({ ok: true, message: "Settings saved and applied." });
        setSlackWebhook("");
        setClearSlack(false);
        setPassword("");
        setClearPassword(false);
      } else {
        setStatus({ ok: false, message: result.error });
      }
    });
  }

  function sendTest() {
    setStatus(null);
    setTestResults(null);
    startTest(async () => {
      const result = await testAlertAction();
      if (result.ok) {
        setTestResults(result.results ?? []);
      } else {
        setStatus({ ok: false, message: result.error });
      }
    });
  }

  return (
    <div className="space-y-6">
      <Card>
        <CardHeader>
          <CardTitle>Slack</CardTitle>
          <CardDescription>
            Deliver alerts to a Slack channel via an{" "}
            <a
              href="https://api.slack.com/messaging/webhooks"
              target="_blank"
              rel="noreferrer"
              className="underline underline-offset-2"
            >
              incoming webhook
            </a>
            .
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-4">
          <div className="space-y-1.5">
            <Label htmlFor="slack_webhook_url">Webhook URL</Label>
            <Input
              id="slack_webhook_url"
              type="url"
              value={slackWebhook}
              onChange={(e) => setSlackWebhook(e.target.value)}
              placeholder={slackSet ? "•••••••• (configured — leave blank to keep)" : "https://hooks.slack.com/services/…"}
              disabled={clearSlack}
            />
          </div>
          {slackSet && (
            <label className="flex items-center gap-2 text-sm text-muted-foreground">
              <Checkbox checked={clearSlack} onChange={setClearSlack} />
              Remove the configured Slack webhook
            </label>
          )}
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>Email</CardTitle>
          <CardDescription>Deliver alerts over SMTP to one or more recipients.</CardDescription>
        </CardHeader>
        <CardContent className="space-y-4">
          <label className="flex items-center gap-2 text-sm">
            <Checkbox checked={emailEnabled} onChange={setEmailEnabled} />
            Enable email alerts
          </label>
          {emailEnabled && (
            <div className="grid gap-4 md:grid-cols-2">
              <div className="space-y-1.5">
                <Label htmlFor="smtp_host">SMTP host</Label>
                <Input
                  id="smtp_host"
                  value={smtpHost}
                  onChange={(e) => setSmtpHost(e.target.value)}
                  placeholder="smtp.example.com"
                />
              </div>
              <div className="space-y-1.5">
                <Label htmlFor="smtp_port">SMTP port</Label>
                <Input
                  id="smtp_port"
                  type="number"
                  value={smtpPort}
                  onChange={(e) => setSmtpPort(e.target.value)}
                  placeholder="587"
                />
              </div>
              <div className="space-y-1.5">
                <Label htmlFor="smtp_user">Username (optional)</Label>
                <Input
                  id="smtp_user"
                  value={username}
                  onChange={(e) => setUsername(e.target.value)}
                  placeholder="apikey"
                  autoComplete="off"
                />
              </div>
              <div className="space-y-1.5">
                <Label htmlFor="smtp_password">Password (optional)</Label>
                <Input
                  id="smtp_password"
                  type="password"
                  value={password}
                  onChange={(e) => setPassword(e.target.value)}
                  placeholder={passwordSet ? "•••••••• (configured — leave blank to keep)" : ""}
                  autoComplete="new-password"
                  disabled={clearPassword}
                />
                {passwordSet && (
                  <label className="flex items-center gap-2 pt-1 text-xs text-muted-foreground">
                    <Checkbox
                      checked={clearPassword}
                      onChange={setClearPassword}
                      className="h-3.5 w-3.5"
                    />
                    Remove the stored password
                  </label>
                )}
              </div>
              <div className="space-y-1.5">
                <Label htmlFor="from_address">From address</Label>
                <Input
                  id="from_address"
                  type="email"
                  value={fromAddress}
                  onChange={(e) => setFromAddress(e.target.value)}
                  placeholder="alerts@example.com"
                />
              </div>
              <div className="space-y-1.5">
                <Label htmlFor="recipients">Recipients</Label>
                <Input
                  id="recipients"
                  value={recipients}
                  onChange={(e) => setRecipients(e.target.value)}
                  placeholder="oncall@example.com, sre@example.com"
                />
              </div>
              <div className="md:col-span-2">
                <label className="flex items-center gap-2 text-sm">
                  <Checkbox checked={starttls} onChange={setStarttls} />
                  Use STARTTLS (recommended; uncheck only for plaintext relays)
                </label>
              </div>
            </div>
          )}
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>Delivery</CardTitle>
          <CardDescription>
            Repeat alerts for the same issue are suppressed within the cooldown window.
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-4">
          <div className="max-w-xs space-y-1.5">
            <Label htmlFor="min_interval_secs">Alert cooldown (seconds)</Label>
            <Input
              id="min_interval_secs"
              type="number"
              min={0}
              value={minInterval}
              onChange={(e) => setMinInterval(e.target.value)}
            />
          </div>
        </CardContent>
      </Card>

      <div className="flex flex-wrap items-center gap-3">
        <Button onClick={save} disabled={pending}>
          {pending ? "Saving…" : "Save settings"}
        </Button>
        <Button variant="outline" onClick={sendTest} disabled={testing}>
          <Send className="mr-2 h-4 w-4" />
          {testing ? "Sending…" : "Send test alert"}
        </Button>
      </div>

      {status && (
        <p
          className={
            status.ok
              ? "rounded-md border border-emerald-500/40 bg-emerald-500/10 px-3 py-2 text-sm text-emerald-600 dark:text-emerald-400"
              : "rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive"
          }
        >
          {status.message}
        </p>
      )}

      {testResults && (
        <Card>
          <CardHeader>
            <CardTitle>Test results</CardTitle>
          </CardHeader>
          <CardContent className="space-y-2">
            {testResults.length === 0 && (
              <p className="text-sm text-muted-foreground">No channels were configured.</p>
            )}
            {testResults.map((r) => (
              <div key={r.channel} className="flex items-start gap-2 text-sm">
                <span
                  className={
                    r.ok
                      ? "mt-0.5 inline-block h-2 w-2 shrink-0 rounded-full bg-emerald-500"
                      : "mt-0.5 inline-block h-2 w-2 shrink-0 rounded-full bg-destructive"
                  }
                />
                <span className="font-medium capitalize">{r.channel}:</span>
                <span className="text-muted-foreground">{r.detail}</span>
              </div>
            ))}
          </CardContent>
        </Card>
      )}
    </div>
  );
}

function ScoringSlider({
  id,
  label,
  hint,
  value,
  onChange,
  min,
  max,
  step,
  valueLabel,
}: {
  id: string;
  label: string;
  hint: string;
  value: number;
  onChange: (value: number) => void;
  min: number;
  max: number;
  step: number;
  valueLabel?: string;
}) {
  return (
    <div className="space-y-1.5">
      <div className="flex items-center justify-between gap-2">
        <Label htmlFor={id}>{label}</Label>
        <span className="text-xs font-medium tabular-nums text-foreground">
          {valueLabel ?? value.toFixed(2)}
        </span>
      </div>
      <input
        id={id}
        type="range"
        min={min}
        max={max}
        step={step}
        value={value}
        onChange={(e) => onChange(Number(e.target.value))}
        className="h-1.5 w-full cursor-pointer appearance-none rounded-full bg-muted accent-foreground"
      />
      <p className="text-xs text-muted-foreground">{hint}</p>
    </div>
  );
}

// Auto-router policy profiles: one click sets every scoring weight and tiering
// choice below. "Custom" is what the form reports once any advanced value no
// longer matches a preset.
type RouterProfileValues = {
  capacity_weight: number;
  cost_weight: number;
  tag_weight: number;
  temperature: number;
  soft_cap: number;
  difficulty: boolean;
  tier_source: "hybrid" | "derived" | "declared";
};

const ROUTER_PROFILES: (SettingsProfile & { values: RouterProfileValues })[] = [
  {
    key: "balanced",
    label: "Balanced",
    recommended: true,
    card: "Weigh spare capacity and price about evenly; topic match breaks ties.",
    blurb:
      "The server defaults: spare capacity matters most, price second, topic match as a strong tiebreaker. Deterministic — the top scorer always wins.",
    values: {
      capacity_weight: 0.6,
      cost_weight: 0.4,
      tag_weight: 0.5,
      temperature: 0,
      soft_cap: 8,
      difficulty: false,
      tier_source: "hybrid",
    },
  },
  {
    key: "best_answer",
    label: "Best answer",
    card: "Follow the topic match and send harder requests to stronger models. Price barely counts.",
    blurb:
      "Topic match dominates scoring and difficulty tiering is on, so harder requests land on stronger models regardless of price. Use when answer quality is worth paying for.",
    values: {
      capacity_weight: 0.4,
      cost_weight: 0.05,
      tag_weight: 0.9,
      temperature: 0,
      soft_cap: 8,
      difficulty: true,
      tier_source: "hybrid",
    },
  },
  {
    key: "cost_saver",
    label: "Cost saver",
    card: "Prefer the cheapest capable model; capacity steers around busy ones.",
    blurb:
      "Price dominates scoring, spare capacity still steers around overloaded models, and topic match only breaks ties. Use when the fleet is billing-sensitive.",
    values: {
      capacity_weight: 0.5,
      cost_weight: 0.9,
      tag_weight: 0.4,
      temperature: 0,
      soft_cap: 8,
      difficulty: false,
      tier_source: "hybrid",
    },
  },
];

function routerProfileFromSettings(s: AutoRouterSettingsView | null): string {
  for (const p of ROUTER_PROFILES) {
    const v = p.values;
    if (
      (s?.capacity_weight ?? 0.6) === v.capacity_weight &&
      (s?.cost_weight ?? 0.4) === v.cost_weight &&
      (s?.tag_weight ?? 0.5) === v.tag_weight &&
      (s?.temperature ?? 0) === v.temperature &&
      (s?.default_soft_cap ?? 8) === v.soft_cap &&
      (s?.difficulty_enabled ?? false) === v.difficulty &&
      (s?.tier_source ?? "hybrid") === v.tier_source
    ) {
      return p.key;
    }
  }
  return "custom";
}

export function AutoRouterSettingsForm({
  settings,
  models,
}: {
  settings: AutoRouterSettingsView | null;
  models: ModelRoute[];
}) {
  const [pending, start] = useTransition();
  const [status, setStatus] = useState<{ ok: boolean; message: string } | null>(null);
  const [enabled, setEnabled] = useState(settings?.classifier_enabled ?? false);
  const [model, setModel] = useState(settings?.classifier_model ?? "");
  const [timeout, setTimeoutMs] = useState(String(settings?.classifier_timeout_ms ?? 250));
  const [capacityWeight, setCapacityWeight] = useState(settings?.capacity_weight ?? 0.6);
  const [costWeight, setCostWeight] = useState(settings?.cost_weight ?? 0.4);
  const [tagWeight, setTagWeight] = useState(settings?.tag_weight ?? 0.5);
  const [softCap, setSoftCap] = useState(String(settings?.default_soft_cap ?? 8));
  const [temperature, setTemperature] = useState(settings?.temperature ?? 0);
  const [difficultyEnabled, setDifficultyEnabled] = useState(
    settings?.difficulty_enabled ?? false,
  );
  const [tierSource, setTierSource] = useState<"hybrid" | "derived" | "declared">(
    settings?.tier_source ?? "hybrid",
  );
  const [profile, setProfile] = useState<string>(() => routerProfileFromSettings(settings));
  const [advanced, setAdvanced] = useState(false);
  const [messagesDefault, setMessagesDefault] = useState(settings?.messages_default_model ?? "");

  function applyProfile(key: string) {
    setProfile(key);
    const preset = ROUTER_PROFILES.find((p) => p.key === key);
    if (!preset) {
      // "Custom" keeps the current values and just opens the advanced editor.
      setAdvanced(true);
      return;
    }
    const v = preset.values;
    setCapacityWeight(v.capacity_weight);
    setCostWeight(v.cost_weight);
    setTagWeight(v.tag_weight);
    setTemperature(v.temperature);
    setSoftCap(String(v.soft_cap));
    setDifficultyEnabled(v.difficulty);
    setTierSource(v.tier_source);
  }

  // Any hand edit to a scoring value means the presets no longer describe it.
  function custom<T>(set: (value: T) => void): (value: T) => void {
    return (value) => {
      setProfile("custom");
      set(value);
    };
  }

  function save() {
    setStatus(null);
    const body: UpdateAutoRouterSettings = {
      classifier_enabled: enabled,
      classifier_model: model.trim() ? model.trim() : "",
      classifier_timeout_ms: Number(timeout) || 250,
      capacity_weight: capacityWeight,
      cost_weight: costWeight,
      tag_weight: tagWeight,
      default_soft_cap: Number(softCap) || 8,
      temperature,
      difficulty_enabled: difficultyEnabled,
      tier_source: tierSource,
      messages_default_model: messagesDefault.trim() ? messagesDefault.trim() : "",
    };
    start(async () => {
      const result = await setAutoRouterSettingsAction(body);
      setStatus(
        result.ok
          ? { ok: true, message: "Auto-router settings saved." }
          : { ok: false, message: result.error },
      );
    });
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle>Auto routing</CardTitle>
        <CardDescription>
          When a client sends <code>model: &quot;auto&quot;</code>, the gateway picks the best model.
          Optionally use a small, fast classifier model to derive intent tags; when disabled or
          unavailable, routing falls back to heuristics then capacity/cost.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        <ToggleRow
          label="Use an intent classifier"
          hint="A tiny model tags each auto request by topic so routing can match models to what the request is actually about. Off = heuristics, then capacity and cost."
          checked={enabled}
          onChange={() => setEnabled((value) => !value)}
        />
        <div className="grid gap-4 sm:grid-cols-2">
          <div className="space-y-1">
            <Label htmlFor="classifier_model">Classifier model</Label>
            <Select
              id="classifier_model"
              value={model}
              onValueChange={setModel}
              searchPlaceholder="Filter models"
              options={[
                { value: "", label: "None" },
                ...models
                  .filter((m) => m.model_name !== "auto")
                  .map((m) => ({ value: m.model_name, label: m.model_name })),
              ]}
            />
            <p className="text-[11px] text-muted-foreground">
              A tiny non-thinking model; anything slower delays every auto request.
            </p>
            {(() => {
              // One-click brain: the cheapest enabled non-reasoning chat model
              // already registered. Fresh installs should not have to know
              // which of their models makes a good classifier.
              const suggestion = models
                .filter(
                  (m) =>
                    m.enabled &&
                    m.model_type === "chat" &&
                    m.model_name !== "auto" &&
                    !(m.tags ?? []).some((t) => t === "reasoning" || t.startsWith("reasoning:")),
                )
                .sort(
                  (a, b) =>
                    a.input_cost_per_token +
                    a.output_cost_per_token -
                    (b.input_cost_per_token + b.output_cost_per_token),
                )[0];
              if (!suggestion || suggestion.model_name === model.trim()) return null;
              return (
                <button
                  type="button"
                  onClick={() => {
                    setModel(suggestion.model_name);
                    setEnabled(true);
                  }}
                  className="text-[11px] font-medium text-primary underline-offset-2 hover:underline"
                >
                  Suggest: use {suggestion.model_name} (cheapest fast model)
                </button>
              );
            })()}
          </div>
          <div className="space-y-1">
            <Label htmlFor="messages_default_model">Default model for Anthropic clients</Label>
            <Select
              id="messages_default_model"
              value={messagesDefault}
              onValueChange={setMessagesDefault}
              searchPlaceholder="Filter models"
              options={[
                { value: "", label: "None" },
                ...(models.some((m) => m.model_name === "auto")
                  ? []
                  : [{ value: "auto", label: "auto" }]),
                ...models.map((m) => ({ value: m.model_name, label: m.model_name })),
              ]}
            />
            <p className="text-[11px] text-muted-foreground">
              Served when a request on /v1/messages names a model the gateway does not know. None
              returns not_found_error.
            </p>
          </div>
          <div className="space-y-1">
            <Label htmlFor="classifier_timeout_ms">Timeout (ms)</Label>
            <Input
              id="classifier_timeout_ms"
              type="number"
              value={timeout}
              onChange={(e) => setTimeoutMs(e.target.value)}
            />
            <p className="text-[11px] text-muted-foreground">
              On timeout the request routes without tags rather than failing.
            </p>
          </div>
        </div>

        <ProfilePicker
          label="Routing profile"
          profiles={ROUTER_PROFILES}
          active={profile}
          onSelect={applyProfile}
          customCard="Hand-tuned scoring weights or tiering — edit them under Advanced."
          customBlurb="The values under Advanced no longer match a preset. Picking a profile above overwrites them."
        />

        <AdvancedDisclosure
          label="Advanced — scoring weights, spread, difficulty tiering"
          open={advanced}
          onToggle={() => setAdvanced((value) => !value)}
        >
          <p className="text-xs text-muted-foreground">
            Weights used to rank candidates for an <code>auto</code> request. Changes apply within
            15 seconds, no restart required.
          </p>

          <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
            <ScoringSlider
              id="capacity_weight"
              label="Capacity"
              hint="How much idle capacity matters when choosing between models."
              value={capacityWeight}
              onChange={custom(setCapacityWeight)}
              min={0}
              max={1}
              step={0.05}
            />
            <ScoringSlider
              id="cost_weight"
              label="Cost"
              hint="How much price matters when choosing between models."
              value={costWeight}
              onChange={custom(setCostWeight)}
              min={0}
              max={1}
              step={0.05}
            />
            <ScoringSlider
              id="tag_weight"
              label="Tag"
              hint="How much a topic match matters, relative to capacity and cost."
              value={tagWeight}
              onChange={custom(setTagWeight)}
              min={0}
              max={1}
              step={0.05}
            />
          </div>

          <div className="grid gap-4 sm:grid-cols-2">
            <ScoringSlider
              id="temperature"
              label="Temperature"
              hint="0 always picks the top-scoring model. Higher values spread traffic across close scorers."
              value={temperature}
              onChange={custom(setTemperature)}
              min={0}
              max={2}
              step={0.1}
              valueLabel={temperature === 0 ? "Deterministic" : temperature.toFixed(1)}
            />
            <div className="space-y-1">
              <Label htmlFor="default_soft_cap">Default soft cap</Label>
              <Input
                id="default_soft_cap"
                type="number"
                min={1}
                value={softCap}
                onChange={(e) => custom(setSoftCap)(e.target.value)}
              />
              <p className="text-xs text-muted-foreground">
                Assumed concurrency ceiling for models with no explicit max in-flight limit.
              </p>
            </div>
          </div>

          <ToggleRow
            label="Difficulty tiering"
            hint="Route harder requests to stronger models. Strength is ranked by price within each topic unless a model declares its own level."
            checked={difficultyEnabled}
            onChange={() => {
              setProfile("custom");
              setDifficultyEnabled((value) => !value);
            }}
          />

          <div className="max-w-xs space-y-1">
            <Label htmlFor="tier_source">Tier source</Label>
            <Select
              id="tier_source"
              value={tierSource}
              onValueChange={(value) =>
                custom(setTierSource)(value as "hybrid" | "derived" | "declared")
              }
              options={[
                { value: "hybrid", label: "Hybrid" },
                { value: "derived", label: "Derived from cost" },
                { value: "declared", label: "Declared only" },
              ]}
            />
          </div>
        </AdvancedDisclosure>

        <Button onClick={save} disabled={pending}>
          {pending ? "Saving..." : "Save auto routing"}
        </Button>
        {status && (
          <p
            className={
              status.ok
                ? "rounded-md border border-emerald-500/40 bg-emerald-500/10 px-3 py-2 text-sm text-emerald-600 dark:text-emerald-400"
                : "rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive"
            }
          >
            {status.message}
          </p>
        )}
      </CardContent>
    </Card>
  );
}

type BoonSectionKey = "vision" | "structured" | "tool_loop" | "image_generation" | "speculation";

function ToggleSwitch({
  checked,
  onChange,
  disabled,
  label,
}: {
  checked: boolean;
  onChange: () => void;
  disabled?: boolean;
  label: string;
}) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={label}
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

function BoonStatusBadge({ enabled }: { enabled: boolean }) {
  return (
    <Badge
      className={cn(
        "text-[10px]",
        enabled
          ? "border-emerald-500/40 bg-emerald-500/10 text-emerald-600 dark:text-emerald-400"
          : "border-border bg-muted/30 text-muted-foreground",
      )}
    >
      {enabled ? "enabled" : "disabled"}
    </Badge>
  );
}

function BoonPanel({
  title,
  description,
  icon: Icon,
  enabled,
  expanded,
  onToggle,
  summary,
  children,
}: {
  title: string;
  description: string;
  icon: LucideIcon;
  enabled: boolean;
  expanded: boolean;
  onToggle: () => void;
  summary: ReactNode;
  children: ReactNode;
}) {
  return (
    <section
      className={cn(
        "overflow-hidden rounded-lg border shadow-sm transition-colors",
        expanded
          ? "border-primary/35 bg-muted/25 ring-1 ring-primary/15"
          : "border-border/70 bg-card/35 hover:border-border hover:bg-muted/15",
      )}
    >
      <div className="grid gap-3 p-4 sm:grid-cols-[minmax(0,1fr)_auto] sm:items-start">
        <button
          type="button"
          onClick={onToggle}
          aria-expanded={expanded}
          className="flex min-w-0 gap-3 text-left"
        >
          <span
            className={cn(
              "mt-0.5 flex h-9 w-9 shrink-0 items-center justify-center rounded-md border",
              enabled
                ? "border-primary/35 bg-primary/10 text-primary"
                : "border-border bg-background text-muted-foreground",
            )}
          >
            <Icon className="h-4 w-4" />
          </span>
          <span className="min-w-0">
            <span className="block text-sm font-medium">{title}</span>
            <span className="mt-0.5 block max-w-3xl text-xs leading-snug text-muted-foreground">
              {description}
            </span>
            <span className="mt-2 flex flex-wrap items-center gap-1.5">
              <BoonStatusBadge enabled={enabled} />
              {summary}
            </span>
          </span>
        </button>
        <Button
          type="button"
          size="icon"
          variant="ghost"
          className="h-8 w-8 justify-self-end text-muted-foreground hover:text-foreground"
          onClick={onToggle}
          aria-expanded={expanded}
          title={expanded ? "Collapse" : "Expand"}
        >
          <ChevronDown
            className={cn("h-4 w-4 transition-transform duration-200", expanded && "rotate-180")}
          />
        </Button>
      </div>

      {expanded && <div className="border-t border-border/60 bg-muted/10 p-4">{children}</div>}
    </section>
  );
}

function ToggleRow({
  label,
  hint,
  checked,
  onChange,
}: {
  label: string;
  hint?: string;
  checked: boolean;
  onChange: () => void;
}) {
  return (
    <div className="flex items-center justify-between gap-4 rounded-lg border border-border/70 bg-background/35 px-4 py-3">
      <div className="min-w-0">
        <p className="text-sm font-medium">{label}</p>
        {hint && <p className="mt-0.5 text-[11px] leading-snug text-muted-foreground">{hint}</p>}
      </div>
      <ToggleSwitch checked={checked} onChange={onChange} label={label} />
    </div>
  );
}

// ---------------------------------------------------------------------------
// Shared settings scaffolding: a named-preset chooser plus a collapsed
// "Advanced" disclosure. Each preset sets every policy value in its section at
// once; any hand edit under Advanced flips the selection to "custom".

type SettingsProfile = {
  key: string;
  label: string;
  card: string;
  blurb: string;
  recommended?: boolean;
};

function ProfileCard({
  active,
  onClick,
  children,
}: {
  active: boolean;
  onClick: () => void;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      aria-pressed={active}
      className={cn(
        "rounded-lg border p-3 text-left transition-colors",
        active
          ? "border-primary/50 bg-primary/10 ring-1 ring-primary/20"
          : "border-border/70 bg-background/40 hover:border-border hover:bg-muted/20",
      )}
    >
      {children}
    </button>
  );
}

function ProfilePicker({
  label = "Policy profile",
  profiles,
  active,
  onSelect,
  customCard,
  customBlurb,
}: {
  label?: string;
  profiles: SettingsProfile[];
  active: string;
  onSelect: (key: string) => void;
  customCard: string;
  customBlurb: string;
}) {
  const activePreset = profiles.find((p) => p.key === active);
  return (
    <div className="space-y-2">
      <Label>{label}</Label>
      <div
        className={cn(
          "grid gap-2",
          profiles.length >= 3 ? "sm:grid-cols-2 lg:grid-cols-4" : "sm:grid-cols-3",
        )}
      >
        {profiles.map((p) => (
          <ProfileCard key={p.key} active={active === p.key} onClick={() => onSelect(p.key)}>
            <span className="flex items-center gap-2 text-sm font-medium">
              {p.label}
              {p.recommended && (
                <Badge className="border-primary/40 bg-primary/10 text-[10px] text-primary">
                  recommended
                </Badge>
              )}
            </span>
            <span className="mt-1 block text-[11px] leading-snug text-muted-foreground">
              {p.card}
            </span>
          </ProfileCard>
        ))}
        <ProfileCard active={!activePreset} onClick={() => onSelect("custom")}>
          <span className="block text-sm font-medium">Custom</span>
          <span className="mt-1 block text-[11px] leading-snug text-muted-foreground">
            {customCard}
          </span>
        </ProfileCard>
      </div>
      <p className="text-[11px] leading-snug text-muted-foreground">
        {activePreset ? activePreset.blurb : customBlurb}
      </p>
    </div>
  );
}

function AdvancedDisclosure({
  label,
  open,
  onToggle,
  children,
}: {
  label: string;
  open: boolean;
  onToggle: () => void;
  children: ReactNode;
}) {
  return (
    <div>
      <button
        type="button"
        onClick={onToggle}
        aria-expanded={open}
        className="flex items-center gap-1.5 text-xs font-medium text-muted-foreground transition-colors hover:text-foreground"
      >
        <ChevronDown
          className={cn("h-3.5 w-3.5 transition-transform duration-200", open && "rotate-180")}
        />
        {label}
      </button>
      {open && (
        <div className="mt-3 space-y-4 rounded-lg border border-border/60 bg-background/25 p-4">
          {children}
        </div>
      )}
    </div>
  );
}

// Speculation policy profiles: one click sets every threshold, cadence value,
// and category gate below. "Custom" is not a preset — it is what the panel
// reports once any advanced value no longer matches a preset.
type SpecProfileKey = "calibrated" | "cautious" | "custom";

type SpecProfileValues = {
  agree_min: number;
  lp_min: number;
  abort_agree: number;
  abort_lp: number;
  first_chunk_tokens: number;
  chunk_tokens: number;
  decide_by_tokens: number;
  max_draft_tokens: number;
  pace_ms: number;
  timeout_ms: number;
  unlisted: boolean;
  gates: SpeculationCategoryGate[];
};

const SPEC_PROFILES: (SettingsProfile & {
  key: Exclude<SpecProfileKey, "custom">;
  values: SpecProfileValues;
})[] = [
  {
    key: "calibrated",
    label: "Calibrated",
    recommended: true,
    card: "Per-category gates from the judged-prompt calibration. Best coverage at high precision.",
    blurb:
      "Drafts are attempted only in the categories where verification measured reliably precise (coding, math, summaries, prose), each with its own tuned floor; categories that never verify well go straight to the target before any draft cost. Requires a classify model.",
    values: {
      agree_min: 0.5,
      lp_min: -1.0,
      abort_agree: 0.45,
      abort_lp: -1.6,
      first_chunk_tokens: 80,
      chunk_tokens: 250,
      decide_by_tokens: 450,
      max_draft_tokens: 2048,
      pace_ms: 9,
      timeout_ms: 45000,
      unlisted: false,
      gates: [
        { tag: "infrastructure", speculate: false },
        { tag: "planning", speculate: false },
        { tag: "database", speculate: false },
        { tag: "regex", speculate: false },
        { tag: "explanation", speculate: false },
        { tag: "debugging", speculate: false },
        { tag: "coding", speculate: true, agree_min: 0.4, lp_min: -0.8 },
        { tag: "math", speculate: true, agree_min: 0.4, lp_min: -0.9 },
        { tag: "summarization", speculate: true, agree_min: 0.4, lp_min: -1.0 },
        { tag: "writing", speculate: true, agree_min: 0.4, lp_min: -1.9 },
      ],
    },
  },
  {
    key: "cautious",
    label: "Cautious",
    card: "One strict global gate, no category routing. Works without a classify model.",
    blurb:
      "Every request is drafted and held to the same strict floor (the server defaults). Fewer requests end up shipping from the drafter, but this needs no classifier and no calibration data — a reasonable starting point on a fleet you have not measured yet.",
    values: {
      agree_min: 0.5,
      lp_min: -1.0,
      abort_agree: 0.45,
      abort_lp: -1.6,
      first_chunk_tokens: 80,
      chunk_tokens: 250,
      decide_by_tokens: 450,
      max_draft_tokens: 2048,
      pace_ms: 9,
      timeout_ms: 45000,
      unlisted: true,
      gates: [],
    },
  },
];

// The server stores gate thresholds with concrete defaults (0.5 / -1.0), so a
// preset row that omits them still round-trips equal.
function specGatesEqual(a: SpeculationCategoryGate[], b: SpeculationCategoryGate[]): boolean {
  if (a.length !== b.length) return false;
  return a.every((g, i) => {
    const h = b[i];
    return (
      g.tag === h.tag &&
      (g.speculate ?? true) === (h.speculate ?? true) &&
      (g.agree_min ?? 0.5) === (h.agree_min ?? 0.5) &&
      (g.lp_min ?? -1.0) === (h.lp_min ?? -1.0)
    );
  });
}

function specProfileFromSettings(s: BoonSettingsView | null): SpecProfileKey {
  for (const p of SPEC_PROFILES) {
    const v = p.values;
    if (
      (s?.speculation_agree_min ?? 0.5) === v.agree_min &&
      (s?.speculation_lp_min ?? -1.0) === v.lp_min &&
      (s?.speculation_abort_agree ?? 0.45) === v.abort_agree &&
      (s?.speculation_abort_lp ?? -1.6) === v.abort_lp &&
      (s?.speculation_first_chunk_tokens ?? 80) === v.first_chunk_tokens &&
      (s?.speculation_chunk_tokens ?? 250) === v.chunk_tokens &&
      (s?.speculation_decide_by_tokens ?? 450) === v.decide_by_tokens &&
      (s?.speculation_max_draft_tokens ?? 2048) === v.max_draft_tokens &&
      (s?.speculation_pace_ms ?? 9) === v.pace_ms &&
      (s?.speculation_timeout_ms ?? 45000) === v.timeout_ms &&
      (s?.speculation_unlisted_categories_speculate ?? true) === v.unlisted &&
      specGatesEqual(s?.speculation_category_gates ?? [], v.gates)
    ) {
      return p.key;
    }
  }
  return "custom";
}

function SpecStep({ n, title, text }: { n: number; title: string; text: string }) {
  return (
    <li className="rounded-lg border border-border/60 bg-background/35 p-3">
      <p className="flex items-center gap-2 text-sm font-medium">
        <span className="flex h-5 w-5 items-center justify-center rounded-full bg-primary/15 text-[11px] font-semibold text-primary">
          {n}
        </span>
        {title}
      </p>
      <p className="mt-1.5 text-[11px] leading-snug text-muted-foreground">{text}</p>
    </li>
  );
}

export function BoonsSettingsForm({
  settings,
  models,
}: {
  settings: BoonSettingsView | null;
  models: ModelRoute[];
}) {
  const [pending, start] = useTransition();
  const [status, setStatus] = useState<{ ok: boolean; message: string } | null>(null);
  const [enabled, setEnabled] = useState(settings?.vision_enabled ?? false);
  const [model, setModel] = useState(settings?.vision_fallback_model ?? "");
  const [prompt, setPrompt] = useState(settings?.vision_describe_prompt ?? "");
  const [maxImages, setMaxImages] = useState(String(settings?.vision_max_images ?? 6));
  const [timeout, setTimeoutMs] = useState(String(settings?.vision_timeout_ms ?? 30000));
  const [structuredEnabled, setStructuredEnabled] = useState(
    settings?.structured_output_enabled ?? false,
  );
  const [fixerModel, setFixerModel] = useState(settings?.structured_output_fixer_model ?? "");
  const [repairAttempts, setRepairAttempts] = useState(
    String(settings?.structured_output_max_repair_attempts ?? 1),
  );
  const [repairTimeout, setRepairTimeout] = useState(
    String(settings?.structured_output_timeout_ms ?? 30000),
  );
  const [toolLoopEnabled, setToolLoopEnabled] = useState(settings?.tool_loop_enabled ?? false);
  const [toolLoopMaxTurns, setToolLoopMaxTurns] = useState(
    String(settings?.tool_loop_max_turns ?? 4),
  );
  const [toolLoopTimeout, setToolLoopTimeout] = useState(
    String(settings?.tool_loop_tool_timeout_ms ?? 30000),
  );
  const [toolLoopNudge, setToolLoopNudge] = useState(settings?.tool_loop_nudge ?? "");
  const [imageEnabled, setImageEnabled] = useState(settings?.image_generation_enabled ?? false);
  const [imageModel, setImageModel] = useState(settings?.image_generation_model ?? "");
  const [imageSizes, setImageSizes] = useState(
    (settings?.image_generation_allowed_sizes ?? ["512x512", "1024x1024"]).join(", "),
  );
  const [imageMaxCount, setImageMaxCount] = useState(
    String(settings?.image_generation_max_images_per_request ?? 2),
  );
  const [imageTimeout, setImageTimeout] = useState(
    String(settings?.image_generation_timeout_ms ?? 120000),
  );
  const [imageToolDescription, setImageToolDescription] = useState(
    settings?.image_generation_tool_description ?? "",
  );
  const [specEnabled, setSpecEnabled] = useState(settings?.speculation_enabled ?? false);
  const [specDraftModel, setSpecDraftModel] = useState(settings?.speculation_draft_model ?? "");
  const [specClassifyModel, setSpecClassifyModel] = useState(
    settings?.speculation_classify_model ?? "",
  );
  const [specAgreeMin, setSpecAgreeMin] = useState(String(settings?.speculation_agree_min ?? 0.5));
  const [specLpMin, setSpecLpMin] = useState(String(settings?.speculation_lp_min ?? -1.0));
  const [specAbortAgree, setSpecAbortAgree] = useState(
    String(settings?.speculation_abort_agree ?? 0.45),
  );
  const [specAbortLp, setSpecAbortLp] = useState(String(settings?.speculation_abort_lp ?? -1.6));
  const [specFirstChunk, setSpecFirstChunk] = useState(
    String(settings?.speculation_first_chunk_tokens ?? 80),
  );
  const [specChunk, setSpecChunk] = useState(String(settings?.speculation_chunk_tokens ?? 250));
  const [specDecideBy, setSpecDecideBy] = useState(
    String(settings?.speculation_decide_by_tokens ?? 450),
  );
  const [specMaxDraft, setSpecMaxDraft] = useState(
    String(settings?.speculation_max_draft_tokens ?? 2048),
  );
  const [specPaceMs, setSpecPaceMs] = useState(String(settings?.speculation_pace_ms ?? 9));
  const [specTimeout, setSpecTimeout] = useState(String(settings?.speculation_timeout_ms ?? 45000));
  const [specKwargs, setSpecKwargs] = useState(
    settings?.speculation_draft_chat_template_kwargs
      ? JSON.stringify(settings.speculation_draft_chat_template_kwargs)
      : "",
  );
  const [specUrlTemplate, setSpecUrlTemplate] = useState(
    settings?.speculation_verify_url_template ?? "",
  );
  const [specUnlisted, setSpecUnlisted] = useState(
    settings?.speculation_unlisted_categories_speculate ?? true,
  );
  const [specGates, setSpecGates] = useState<SpeculationCategoryGate[]>(
    settings?.speculation_category_gates ?? [],
  );
  const [specProfile, setSpecProfile] = useState<SpecProfileKey>(() =>
    specProfileFromSettings(settings),
  );
  const [specAdvanced, setSpecAdvanced] = useState(false);
  const [expanded, setExpanded] = useState<BoonSectionKey | null>(null);

  function updateSpecGate(index: number, patch: Partial<SpeculationCategoryGate>) {
    setSpecProfile("custom");
    setSpecGates((gates) => gates.map((g, i) => (i === index ? { ...g, ...patch } : g)));
  }

  function applySpecProfile(key: SpecProfileKey) {
    setSpecProfile(key);
    const preset = SPEC_PROFILES.find((p) => p.key === key);
    if (!preset) {
      // "Custom" keeps the current values and just opens the advanced editor.
      setSpecAdvanced(true);
      return;
    }
    const v = preset.values;
    setSpecAgreeMin(String(v.agree_min));
    setSpecLpMin(String(v.lp_min));
    setSpecAbortAgree(String(v.abort_agree));
    setSpecAbortLp(String(v.abort_lp));
    setSpecFirstChunk(String(v.first_chunk_tokens));
    setSpecChunk(String(v.chunk_tokens));
    setSpecDecideBy(String(v.decide_by_tokens));
    setSpecMaxDraft(String(v.max_draft_tokens));
    setSpecPaceMs(String(v.pace_ms));
    setSpecTimeout(String(v.timeout_ms));
    setSpecUnlisted(v.unlisted);
    setSpecGates(v.gates.map((g) => ({ ...g })));
  }

  // Any hand edit to a policy value means the presets no longer describe it.
  function specCustom(set: (value: string) => void): (value: string) => void {
    return (value) => {
      setSpecProfile("custom");
      set(value);
    };
  }

  const visionModels = models.filter(
    (m) => m.model_name !== "auto" && (m.supports_vision || tagsInclude(m.tags, "vision")),
  );
  const chatModels = models.filter(
    (m) => m.model_name !== "auto" && (m.model_type ?? "chat") === "chat",
  );
  const imageModels = models.filter(
    (m) => m.model_name !== "auto" && m.model_type === "image",
  );

  function toggleSection(section: BoonSectionKey) {
    setExpanded((current) => (current === section ? null : section));
  }

  function save() {
    setStatus(null);
    // Draft kwargs is free-form JSON; refuse to save silently-broken input.
    let specKwargsParsed: Record<string, unknown> | undefined;
    const kwargsText = specKwargs.trim();
    if (kwargsText === "") {
      specKwargsParsed = {}; // empty object clears server-side
    } else {
      try {
        const parsed: unknown = JSON.parse(kwargsText);
        if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
          throw new Error("must be a JSON object");
        }
        specKwargsParsed = parsed as Record<string, unknown>;
      } catch (e) {
        setStatus({
          ok: false,
          message: `Draft chat_template_kwargs is not a JSON object: ${e instanceof Error ? e.message : String(e)}`,
        });
        return;
      }
    }
    const specGatesClean = specGates
      .map((g) => ({ ...g, tag: g.tag.trim() }))
      .filter((g) => g.tag.length > 0);
    const body: UpdateBoonSettings = {
      vision_enabled: enabled,
      vision_fallback_model: model.trim() ? model.trim() : "",
      vision_describe_prompt: prompt.trim(),
      vision_max_images: Number(maxImages) || 6,
      vision_timeout_ms: Number(timeout) || 30000,
      structured_output_enabled: structuredEnabled,
      structured_output_fixer_model: fixerModel.trim() ? fixerModel.trim() : "",
      structured_output_max_repair_attempts: Math.min(Number(repairAttempts) || 1, 3),
      structured_output_timeout_ms: Number(repairTimeout) || 30000,
      tool_loop_enabled: toolLoopEnabled,
      tool_loop_max_turns: Math.min(Number(toolLoopMaxTurns) || 4, 8),
      tool_loop_tool_timeout_ms: Number(toolLoopTimeout) || 30000,
      tool_loop_nudge: toolLoopNudge,
      image_generation_enabled: imageEnabled,
      image_generation_model: imageModel.trim() ? imageModel.trim() : "",
      image_generation_tool_description: imageToolDescription.trim(),
      image_generation_allowed_sizes: imageSizes
        .split(",")
        .map((s) => s.trim())
        .filter(Boolean),
      image_generation_max_images_per_request: Math.min(Number(imageMaxCount) || 2, 4),
      image_generation_timeout_ms: Number(imageTimeout) || 120000,
      speculation_enabled: specEnabled,
      speculation_draft_model: specDraftModel.trim() ? specDraftModel.trim() : "",
      // Deprecated: verification resolves per target from the model's own
      // scoring endpoint; the global override is retired.
      speculation_verify_model: "",
      speculation_classify_model: specClassifyModel.trim() ? specClassifyModel.trim() : "",
      speculation_agree_min: Number(specAgreeMin) || 0.5,
      speculation_lp_min: Number(specLpMin) || -1.0,
      speculation_abort_agree: Number(specAbortAgree) || 0.45,
      speculation_abort_lp: Number(specAbortLp) || -1.6,
      speculation_first_chunk_tokens: Number(specFirstChunk) || 80,
      speculation_chunk_tokens: Number(specChunk) || 250,
      speculation_decide_by_tokens: Number(specDecideBy) || 450,
      speculation_max_draft_tokens: Number(specMaxDraft) || 2048,
      speculation_pace_ms: Math.max(Number(specPaceMs) || 0, 0),
      speculation_timeout_ms: Number(specTimeout) || 45000,
      speculation_draft_chat_template_kwargs: specKwargsParsed,
      speculation_category_gates: specGatesClean,
      speculation_unlisted_categories_speculate: specUnlisted,
      speculation_verify_url_template: specUrlTemplate.trim(),
    };
    start(async () => {
      const result = await setBoonSettingsAction(body);
      setStatus(
        result.ok
          ? { ok: true, message: "Boon settings saved." }
          : { ok: false, message: result.error },
      );
    });
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          <Sparkles className="h-4 w-4" />
          Model boons
        </CardTitle>
        <CardDescription>
          Gateway-granted capabilities for models that lack them natively. Configure the global
          helpers here, then opt specific models into each boon from the Models page. If a helper
          is unavailable, requests pass through unchanged.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        <div className="space-y-3">
          <BoonPanel
            title="Vision"
            description="Relays image inputs to a describer model and rewrites them as text."
            icon={Eye}
            enabled={enabled}
            expanded={expanded === "vision"}
            onToggle={() => toggleSection("vision")}
            summary={
              <>
                <Badge className="border-border bg-background text-[10px] text-muted-foreground">
                  {model || "no describer"}
                </Badge>
                <Badge className="border-border bg-background text-[10px] text-muted-foreground">
                  {maxImages || "6"} images
                </Badge>
                <Badge className="border-border bg-background text-[10px] text-muted-foreground">
                  {timeout || "30000"} ms
                </Badge>
              </>
            }
          >
            <div className="space-y-4">
              <ToggleRow
                label="Enable vision boon"
                hint="Only opted-in models that lack native vision use this relay."
                checked={enabled}
                onChange={() => setEnabled((value) => !value)}
              />
              <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
                <div className="space-y-1">
                  <Label htmlFor="vision_fallback_model">Describer model</Label>
                  <Select
                    id="vision_fallback_model"
                    value={model}
                    onValueChange={setModel}
                    searchPlaceholder="Filter models"
                    options={[
                      { value: "", label: "None" },
                      ...visionModels.map((m) => ({ value: m.model_name, label: m.model_name })),
                    ]}
                  />
                </div>
                <div className="space-y-1">
                  <Label htmlFor="vision_max_images">Max images per request</Label>
                  <Input
                    id="vision_max_images"
                    type="number"
                    value={maxImages}
                    onChange={(e) => setMaxImages(e.target.value)}
                  />
                </div>
                <div className="space-y-1">
                  <Label htmlFor="vision_timeout_ms">Describe timeout (ms)</Label>
                  <Input
                    id="vision_timeout_ms"
                    type="number"
                    value={timeout}
                    onChange={(e) => setTimeoutMs(e.target.value)}
                  />
                </div>
              </div>
              <div className="space-y-1">
                <Label htmlFor="vision_describe_prompt">Describe prompt</Label>
                <textarea
                  id="vision_describe_prompt"
                  value={prompt}
                  onChange={(e) => setPrompt(e.target.value)}
                  rows={3}
                  className="flex w-full rounded-md border border-input bg-background px-3 py-2 text-sm"
                  placeholder="Describe this image in detail..."
                />
              </div>
            </div>
          </BoonPanel>

          <BoonPanel
            title="Structured output"
            description="Validates response_format JSON and repairs invalid replies."
            icon={Braces}
            enabled={structuredEnabled}
            expanded={expanded === "structured"}
            onToggle={() => toggleSection("structured")}
            summary={
              <>
                <Badge className="border-border bg-background text-[10px] text-muted-foreground">
                  {fixerModel || "same model"}
                </Badge>
                <Badge className="border-border bg-background text-[10px] text-muted-foreground">
                  {repairAttempts || "1"} repairs
                </Badge>
                <Badge className="border-border bg-background text-[10px] text-muted-foreground">
                  {repairTimeout || "30000"} ms
                </Badge>
              </>
            }
          >
            <div className="space-y-4">
              <ToggleRow
                label="Enable structured output boon"
                hint="Applies to opted-in models that lack native response schema support."
                checked={structuredEnabled}
                onChange={() => setStructuredEnabled((value) => !value)}
              />
              <p className="text-sm text-muted-foreground">
                <code>response_format</code> requests are validated at the gateway; invalid JSON is
                repaired by the fixer model or by re-prompting the same model. On final failure the
                original reply passes through with a warning header.
              </p>
              <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
                <div className="space-y-1">
                  <Label htmlFor="structured_output_fixer_model">Fixer model</Label>
                  <Select
                    id="structured_output_fixer_model"
                    value={fixerModel}
                    onValueChange={setFixerModel}
                    searchPlaceholder="Filter models"
                    options={[
                      { value: "", label: "Same model (re-prompt)" },
                      ...chatModels.map((m) => ({ value: m.model_name, label: m.model_name })),
                    ]}
                  />
                </div>
                <div className="space-y-1">
                  <Label htmlFor="structured_output_max_repair_attempts">
                    Max repair attempts (0-3)
                  </Label>
                  <Input
                    id="structured_output_max_repair_attempts"
                    type="number"
                    min={0}
                    max={3}
                    value={repairAttempts}
                    onChange={(e) => setRepairAttempts(e.target.value)}
                  />
                </div>
                <div className="space-y-1">
                  <Label htmlFor="structured_output_timeout_ms">Repair timeout (ms)</Label>
                  <Input
                    id="structured_output_timeout_ms"
                    type="number"
                    value={repairTimeout}
                    onChange={(e) => setRepairTimeout(e.target.value)}
                  />
                </div>
              </div>
            </div>
          </BoonPanel>

          <BoonPanel
            title="MCP tool loop"
            description="Injects granted MCP tools and runs tool calls between model turns."
            icon={Wrench}
            enabled={toolLoopEnabled}
            expanded={expanded === "tool_loop"}
            onToggle={() => toggleSection("tool_loop")}
            summary={
              <>
                <Badge className="border-border bg-background text-[10px] text-muted-foreground">
                  {toolLoopMaxTurns || "4"} turns
                </Badge>
                <Badge className="border-border bg-background text-[10px] text-muted-foreground">
                  {toolLoopTimeout || "30000"} ms
                </Badge>
                <Badge className="border-border bg-background text-[10px] text-muted-foreground">
                  {toolLoopNudge.trim() ? "custom nudge" : "default nudge"}
                </Badge>
              </>
            }
          >
            <div className="space-y-4">
              <ToggleRow
                label="Enable gateway tool loop"
                hint="Models need native function calling and a per-model tool grant."
                checked={toolLoopEnabled}
                onChange={() => setToolLoopEnabled((value) => !value)}
              />
          <p className="text-sm text-muted-foreground">
            Models granted access to registered MCP servers (per model, in the model&apos;s{" "}
            <strong>Tools</strong> section) get those tools injected into plain chat requests;
            the gateway executes the tool calls and loops until the model answers. Requires the
            model&apos;s native <strong>Function calling</strong> capability. Streaming clients get
            a live token stream with a visible search marker; only the tool execution between
            turns pauses the stream. Clients that send their own <code>tools</code> keep control
            of those calls. Tool-loop answers are never cached.
          </p>
          <div className="grid gap-4 sm:grid-cols-2">
            <div className="space-y-1">
              <Label htmlFor="tool_loop_max_turns">Max tool turns (1-8)</Label>
              <Input
                id="tool_loop_max_turns"
                type="number"
                min={1}
                max={8}
                value={toolLoopMaxTurns}
                onChange={(e) => setToolLoopMaxTurns(e.target.value)}
              />
            </div>
            <div className="space-y-1">
              <Label htmlFor="tool_loop_tool_timeout_ms">Tool execution timeout (ms)</Label>
              <Input
                id="tool_loop_tool_timeout_ms"
                type="number"
                value={toolLoopTimeout}
                onChange={(e) => setToolLoopTimeout(e.target.value)}
              />
            </div>
          </div>
          <div className="space-y-1">
            <Label htmlFor="tool_loop_nudge">Tool nudge</Label>
            <textarea
              id="tool_loop_nudge"
              value={toolLoopNudge}
              onChange={(e) => setToolLoopNudge(e.target.value)}
              rows={4}
              className="flex w-full rounded-md border border-input bg-background px-3 py-2 text-sm"
              placeholder="Leave blank to reset to the built-in default..."
            />
            <p className="text-xs text-muted-foreground">
              System instruction injected with the granted tools so the model knows it has them
              and when to call them. Tune it to make a model search more (or less) eagerly. Only
              applied to plain chat clients — clients that send their own <code>tools</code> are
              left untouched. Blank resets to the built-in default.
            </p>
          </div>
            </div>
          </BoonPanel>

          <BoonPanel
            title="Image generation"
            description="Injects a generate_image tool so a chat model can produce images through a registered image model."
            icon={ImageIcon}
            enabled={imageEnabled}
            expanded={expanded === "image_generation"}
            onToggle={() => toggleSection("image_generation")}
            summary={
              <>
                <Badge className="border-border bg-background text-[10px] text-muted-foreground">
                  {imageModel || "no image model"}
                </Badge>
                <Badge className="border-border bg-background text-[10px] text-muted-foreground">
                  max {imageMaxCount || "2"} per call
                </Badge>
                <Badge className="border-border bg-background text-[10px] text-muted-foreground">
                  {imageTimeout || "120000"} ms
                </Badge>
              </>
            }
          >
            <div className="space-y-4">
              <ToggleRow
                label="Enable image generation boon"
                hint="Opted-in models gain a generate_image tool. The model must also have the Function calling capability, or no tool is injected."
                checked={imageEnabled}
                onChange={() => setImageEnabled((value) => !value)}
              />
              <p className="text-sm text-muted-foreground">
                The model decides when to call the tool. The gateway runs the generation against
                the image model below, bills it per image, and attaches the result to the reply.
                Requests that also ask for a <code>response_format</code> schema keep the schema
                and drop the image.
              </p>
              <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
                <div className="space-y-1">
                  <Label htmlFor="image_generation_model">Image model</Label>
                  <Select
                    id="image_generation_model"
                    value={imageModel}
                    onValueChange={setImageModel}
                    searchPlaceholder="Filter models"
                    options={[
                      { value: "", label: "None" },
                      ...imageModels.map((m) => ({ value: m.model_name, label: m.model_name })),
                    ]}
                  />
                </div>
                <div className="space-y-1">
                  <Label htmlFor="image_generation_max_images_per_request">
                    Max images per call (1-4)
                  </Label>
                  <Input
                    id="image_generation_max_images_per_request"
                    type="number"
                    min={1}
                    max={4}
                    value={imageMaxCount}
                    onChange={(e) => setImageMaxCount(e.target.value)}
                  />
                </div>
                <div className="space-y-1">
                  <Label htmlFor="image_generation_timeout_ms">Generation timeout (ms)</Label>
                  <Input
                    id="image_generation_timeout_ms"
                    type="number"
                    value={imageTimeout}
                    onChange={(e) => setImageTimeout(e.target.value)}
                  />
                </div>
              </div>
              <div className="space-y-1">
                <Label htmlFor="image_generation_allowed_sizes">Allowed sizes</Label>
                <Input
                  id="image_generation_allowed_sizes"
                  value={imageSizes}
                  onChange={(e) => setImageSizes(e.target.value)}
                  placeholder="512x512, 1024x1024"
                />
                <p className="text-[11px] text-muted-foreground">
                  Comma separated. Offered to the model in the tool schema and enforced at
                  execution; a size outside the list falls back to the first entry.
                </p>
              </div>
              <div className="space-y-1">
                <Label htmlFor="image_generation_tool_description">Tool description</Label>
                <textarea
                  id="image_generation_tool_description"
                  value={imageToolDescription}
                  onChange={(e) => setImageToolDescription(e.target.value)}
                  rows={3}
                  className="flex w-full rounded-md border border-input bg-background px-3 py-2 text-sm"
                  placeholder="Generate an image from a text description..."
                />
                <p className="text-[11px] text-muted-foreground">
                  What the model reads when deciding to call the tool. Clear it to restore the
                  default.
                </p>
              </div>
            </div>
          </BoonPanel>

          <BoonPanel
            title="Speculation"
            description="Answers with a fast drafter whenever the target model itself verifies the draft; unverified drafts fall through to the target."
            icon={Rabbit}
            enabled={specEnabled}
            expanded={expanded === "speculation"}
            onToggle={() => toggleSection("speculation")}
            summary={
              <>
                <Badge className="border-border bg-background text-[10px] text-muted-foreground">
                  {specDraftModel || "no drafter"}
                </Badge>
                <Badge className="border-border bg-background text-[10px] text-muted-foreground">
                  {specProfile === "custom"
                    ? "custom policy"
                    : `${SPEC_PROFILES.find((p) => p.key === specProfile)?.label ?? specProfile} profile`}
                </Badge>
              </>
            }
          >
            <div className="space-y-4">
              <ToggleRow
                label="Enable speculation boon"
                hint="Applies to opted-in models. Nothing reaches the client before it is verified, so a failed draft only costs latency, never quality."
                checked={specEnabled}
                onChange={() => setSpecEnabled((value) => !value)}
              />
              <ol className="grid gap-2 sm:grid-cols-3">
                <SpecStep
                  n={1}
                  title="Classify"
                  text="A tiny model tags the request. Categories where drafting never pays off skip straight to the target — before any draft cost."
                />
                <SpecStep
                  n={2}
                  title="Draft"
                  text="The drafter writes the whole answer at its own, much faster, speed."
                />
                <SpecStep
                  n={3}
                  title="Verify, then release"
                  text="The target model scores every draft token in one cheap pass. Only verified text reaches the client; anything else falls through to the target seamlessly."
                />
              </ol>
              <p className="text-[11px] text-muted-foreground">
                Each model configures its own cascade on the Models page: tick Speculation there,
                pick its drafter, and set its scoring endpoint. This panel holds only the fleet
                defaults and thresholds.
              </p>
              <div className="grid gap-4 sm:grid-cols-2">
                <div className="space-y-1">
                  <Label htmlFor="speculation_draft_model">Default drafter — writes the answer</Label>
                  <Select
                    id="speculation_draft_model"
                    value={specDraftModel}
                    onValueChange={setSpecDraftModel}
                    searchPlaceholder="Filter models"
                    options={[
                      { value: "", label: "None" },
                      ...chatModels.map((m) => ({ value: m.model_name, label: m.model_name })),
                    ]}
                  />
                  <p className="text-[11px] text-muted-foreground">
                    Fleet default; a model can pick its own drafter on the Models page. Needs a
                    5-10x speed gap over the target to pay off.
                  </p>
                </div>
                <div className="space-y-1">
                  <Label htmlFor="speculation_classify_model">Classifier — triage only</Label>
                  <Select
                    id="speculation_classify_model"
                    value={specClassifyModel}
                    onValueChange={setSpecClassifyModel}
                    searchPlaceholder="Filter models"
                    options={[
                      { value: "", label: "None (one gate for everything)" },
                      ...chatModels.map((m) => ({ value: m.model_name, label: m.model_name })),
                    ]}
                  />
                  <p className="text-[11px] text-muted-foreground">
                    A tiny non-thinking model that tags each request. Needed by the Calibrated
                    profile; with None every request uses the same gate.
                  </p>
                </div>
              </div>
              <ProfilePicker
                profiles={SPEC_PROFILES}
                active={specProfile}
                onSelect={(key) => applySpecProfile(key as SpecProfileKey)}
                customCard="Hand-tuned floors, cadence, or category gates — edit them under Advanced."
                customBlurb="The values under Advanced no longer match a preset. Picking a profile above overwrites them."
              />
              <AdvancedDisclosure
                label="Advanced — scoring endpoint rule, decision floors, verify cadence, category gates"
                open={specAdvanced}
                onToggle={() => setSpecAdvanced((value) => !value)}
              >
                <div className="space-y-1">
                  <Label htmlFor="speculation_verify_url_template">Scoring endpoint rule</Label>
                  <Input
                    id="speculation_verify_url_template"
                    value={specUrlTemplate}
                    onChange={(e) => setSpecUrlTemplate(e.target.value)}
                    placeholder="http://{upstream}.serving.svc.cluster.local:8000/v1"
                  />
                  <p className="text-[11px] text-muted-foreground">
                    How the gateway finds a speculating model&apos;s scoring endpoint when the
                    model doesn&apos;t set its own: <code>{"{upstream}"}</code> and{" "}
                    <code>{"{model}"}</code> expand per model. Set this only once the fleet&apos;s
                    backends survive <code>prompt_logprobs</code> scoring — pointing it at
                    unpatched pods kills them. Blank = models without their own endpoint
                    don&apos;t speculate.
                  </p>
                </div>
              <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-4">
                <div className="space-y-1">
                  <Label htmlFor="speculation_agree_min">Ship floor: agreement (0-1)</Label>
                  <Input
                    id="speculation_agree_min"
                    type="number"
                    step="0.05"
                    min={0}
                    max={1}
                    value={specAgreeMin}
                    onChange={(e) => specCustom(setSpecAgreeMin)(e.target.value)}
                  />
                </div>
                <div className="space-y-1">
                  <Label htmlFor="speculation_lp_min">Ship floor: mean logprob</Label>
                  <Input
                    id="speculation_lp_min"
                    type="number"
                    step="0.1"
                    max={0}
                    value={specLpMin}
                    onChange={(e) => specCustom(setSpecLpMin)(e.target.value)}
                  />
                </div>
                <div className="space-y-1">
                  <Label htmlFor="speculation_abort_agree">Abort floor: agreement</Label>
                  <Input
                    id="speculation_abort_agree"
                    type="number"
                    step="0.05"
                    min={0}
                    max={1}
                    value={specAbortAgree}
                    onChange={(e) => specCustom(setSpecAbortAgree)(e.target.value)}
                  />
                </div>
                <div className="space-y-1">
                  <Label htmlFor="speculation_abort_lp">Abort floor: mean logprob</Label>
                  <Input
                    id="speculation_abort_lp"
                    type="number"
                    step="0.1"
                    max={0}
                    value={specAbortLp}
                    onChange={(e) => specCustom(setSpecAbortLp)(e.target.value)}
                  />
                </div>
              </div>
              <p className="text-[11px] text-muted-foreground">
                Between the abort floors and the ship floors the decision is deferred while the
                draft grows — a draft&apos;s opening is its lowest-scoring region.
              </p>
              <div className="grid gap-4 sm:grid-cols-2 lg:grid-cols-3">
                <div className="space-y-1">
                  <Label htmlFor="speculation_first_chunk_tokens">First verify at (tokens)</Label>
                  <Input
                    id="speculation_first_chunk_tokens"
                    type="number"
                    value={specFirstChunk}
                    onChange={(e) => specCustom(setSpecFirstChunk)(e.target.value)}
                  />
                </div>
                <div className="space-y-1">
                  <Label htmlFor="speculation_chunk_tokens">Then every (tokens)</Label>
                  <Input
                    id="speculation_chunk_tokens"
                    type="number"
                    value={specChunk}
                    onChange={(e) => specCustom(setSpecChunk)(e.target.value)}
                  />
                </div>
                <div className="space-y-1">
                  <Label htmlFor="speculation_decide_by_tokens">Defer patience (tokens)</Label>
                  <Input
                    id="speculation_decide_by_tokens"
                    type="number"
                    value={specDecideBy}
                    onChange={(e) => specCustom(setSpecDecideBy)(e.target.value)}
                  />
                </div>
                <div className="space-y-1">
                  <Label htmlFor="speculation_max_draft_tokens">Draft token cap</Label>
                  <Input
                    id="speculation_max_draft_tokens"
                    type="number"
                    value={specMaxDraft}
                    onChange={(e) => specCustom(setSpecMaxDraft)(e.target.value)}
                  />
                </div>
                <div className="space-y-1">
                  <Label htmlFor="speculation_pace_ms">Release pacing (ms/token)</Label>
                  <Input
                    id="speculation_pace_ms"
                    type="number"
                    min={0}
                    value={specPaceMs}
                    onChange={(e) => specCustom(setSpecPaceMs)(e.target.value)}
                  />
                </div>
                <div className="space-y-1">
                  <Label htmlFor="speculation_timeout_ms">Pre-release budget (ms)</Label>
                  <Input
                    id="speculation_timeout_ms"
                    type="number"
                    value={specTimeout}
                    onChange={(e) => specCustom(setSpecTimeout)(e.target.value)}
                  />
                </div>
              </div>
              <div className="space-y-1">
                <Label htmlFor="speculation_draft_chat_template_kwargs">
                  Draft chat_template_kwargs (JSON)
                </Label>
                <Input
                  id="speculation_draft_chat_template_kwargs"
                  value={specKwargs}
                  onChange={(e) => setSpecKwargs(e.target.value)}
                  placeholder='{"reasoning": false}'
                />
                <p className="text-[11px] text-muted-foreground">
                  Sent with draft calls only (e.g. to turn a drafter&apos;s thinking off — measured
                  faster and more reliable). Blank clears it.
                </p>
              </div>
              <div className="space-y-2">
                <div className="flex items-center justify-between">
                  <Label>Category gates</Label>
                  <Button
                    type="button"
                    variant="outline"
                    size="sm"
                    onClick={() => {
                      setSpecProfile("custom");
                      setSpecGates((gates) => [
                        ...gates,
                        { tag: "", speculate: true, agree_min: 0.5, lp_min: -1.0 },
                      ]);
                    }}
                  >
                    <Plus className="h-3.5 w-3.5" />
                    Add category
                  </Button>
                </div>
                <p className="text-[11px] text-muted-foreground">
                  The classify model maps each request onto these tags; the first matching row
                  wins, so list excluded categories first. &quot;Speculate&quot; off = the target
                  answers directly, before any draft cost.
                </p>
                {specGates.length === 0 && (
                  <p className="rounded-md border border-border bg-muted/30 px-3 py-2 text-sm text-muted-foreground">
                    No category gates: the global ship floors above apply to every request.
                  </p>
                )}
                {specGates.map((gate, i) => (
                  <div
                    key={i}
                    className="grid items-end gap-2 rounded-md border border-border/60 bg-muted/20 p-2 sm:grid-cols-[1fr_auto_auto_auto_auto]"
                  >
                    <div className="space-y-1">
                      <Label htmlFor={`spec_gate_tag_${i}`} className="text-[11px]">
                        Tag
                      </Label>
                      <Input
                        id={`spec_gate_tag_${i}`}
                        value={gate.tag}
                        onChange={(e) => updateSpecGate(i, { tag: e.target.value })}
                        placeholder="coding"
                      />
                    </div>
                    <div className="space-y-1">
                      <Label className="text-[11px]">Speculate</Label>
                      <div className="flex h-9 items-center">
                        <Checkbox
                          checked={gate.speculate ?? true}
                          onChange={(checked) => updateSpecGate(i, { speculate: checked })}
                          aria-label={`Speculate for ${gate.tag || "this category"}`}
                        />
                      </div>
                    </div>
                    <div className="space-y-1">
                      <Label htmlFor={`spec_gate_agree_${i}`} className="text-[11px]">
                        Agreement ≥
                      </Label>
                      <Input
                        id={`spec_gate_agree_${i}`}
                        type="number"
                        step="0.05"
                        min={0}
                        max={1}
                        className="w-24"
                        disabled={!(gate.speculate ?? true)}
                        value={String(gate.agree_min ?? 0.5)}
                        onChange={(e) => updateSpecGate(i, { agree_min: Number(e.target.value) })}
                      />
                    </div>
                    <div className="space-y-1">
                      <Label htmlFor={`spec_gate_lp_${i}`} className="text-[11px]">
                        Logprob ≥
                      </Label>
                      <Input
                        id={`spec_gate_lp_${i}`}
                        type="number"
                        step="0.1"
                        max={0}
                        className="w-24"
                        disabled={!(gate.speculate ?? true)}
                        value={String(gate.lp_min ?? -1.0)}
                        onChange={(e) => updateSpecGate(i, { lp_min: Number(e.target.value) })}
                      />
                    </div>
                    <Button
                      type="button"
                      variant="ghost"
                      size="sm"
                      onClick={() => {
                        setSpecProfile("custom");
                        setSpecGates((gates) => gates.filter((_, j) => j !== i));
                      }}
                    >
                      <Trash2 className="h-3.5 w-3.5" />
                    </Button>
                  </div>
                ))}
                <ToggleRow
                  label="Unlisted categories use the global gate"
                  hint="Off = requests whose category has no row above never speculate (the target answers directly)."
                  checked={specUnlisted}
                  onChange={() => {
                    setSpecProfile("custom");
                    setSpecUnlisted((value) => !value);
                  }}
                />
              </div>
              </AdvancedDisclosure>
            </div>
          </BoonPanel>

        </div>

        <div className="flex flex-wrap items-center gap-3 border-t border-border/60 pt-4">
          <Button onClick={save} disabled={pending}>
            <Save className="h-4 w-4" />
            {pending ? "Saving..." : "Save boons"}
          </Button>
          {status && (
            <p
              className={
                status.ok
                  ? "rounded-md border border-emerald-500/40 bg-emerald-500/10 px-3 py-2 text-sm text-emerald-600 dark:text-emerald-400"
                  : "rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive"
              }
            >
              {status.message}
            </p>
          )}
        </div>
      </CardContent>
    </Card>
  );
}

function CompressorStatusBlock({ compressor }: { compressor: CompressorStatusView | null }) {
  if (!compressor || !compressor.configured) {
    return (
      <div className="flex items-start gap-2 rounded-md border border-border bg-muted/30 px-3 py-2 text-sm text-muted-foreground">
        <span className="mt-1 inline-block h-2 w-2 shrink-0 rounded-full bg-muted-foreground/50" />
        <span>
          <span className="font-medium text-foreground">Neural sidecar not configured.</span> The
          lossy prose pass uses the built-in heuristic. To enable the trained{" "}
          <code>compressor</code> scorer, deploy the sidecar and set{" "}
          <code>OBLETH_COMPRESSOR_URL</code> (Docker: add <code>compressor</code> to{" "}
          <code>COMPOSE_PROFILES</code>; Kubernetes: <code>compressor.enabled=true</code>).
        </span>
      </div>
    );
  }
  const ok = compressor.reachable;
  return (
    <div
      className={
        ok
          ? "flex items-start gap-2 rounded-md border border-emerald-500/40 bg-emerald-500/10 px-3 py-2 text-sm text-emerald-600 dark:text-emerald-400"
          : "flex items-start gap-2 rounded-md border border-amber-500/40 bg-amber-500/10 px-3 py-2 text-sm text-amber-600 dark:text-amber-400"
      }
    >
      <span
        className={
          ok
            ? "mt-1 inline-block h-2 w-2 shrink-0 rounded-full bg-emerald-500"
            : "mt-1 inline-block h-2 w-2 shrink-0 rounded-full bg-amber-500"
        }
      />
      <span>
        {ok ? (
          <>
            <span className="font-medium">Neural sidecar reachable</span> — model{" "}
            <span className="font-mono">{compressor.model ?? "unknown"}</span>
            {compressor.revision && (
              <span className="ml-1 font-mono text-xs opacity-70">
                {compressor.revision.slice(0, 7)}
              </span>
            )}{" "}
            at <code>{compressor.url}</code>. The lossy prose pass uses the neural scorer.
          </>
        ) : (
          <>
            <span className="font-medium">Neural sidecar unreachable</span> at{" "}
            <code>{compressor.url}</code>
            {compressor.error && <> — {compressor.error}</>}. The gateway falls back to the built-in
            heuristic until it recovers.
          </>
        )}
      </span>
    </div>
  );
}

export function CompressionSettingsForm({
  settings,
  compressor,
}: {
  settings: BoonSettingsView | null;
  compressor: CompressorStatusView | null;
}) {
  const [pending, start] = useTransition();
  const [status, setStatus] = useState<{ ok: boolean; message: string } | null>(null);

  const [enabled, setEnabled] = useState(settings?.compression_enabled ?? false);
  const [codeCompaction, setCodeCompaction] = useState(
    settings?.compression_code_compaction ?? false,
  );
  const [dedup, setDedup] = useState(settings?.compression_dedup ?? false);
  const [compactLogs, setCompactLogs] = useState(settings?.compression_compact_logs ?? false);
  const [allowLossy, setAllowLossy] = useState(settings?.compression_allow_lossy ?? false);
  const [minTokens, setMinTokens] = useState(String(settings?.compression_min_tokens ?? 512));
  const [maxSegments, setMaxSegments] = useState(String(settings?.compression_max_segments ?? 64));
  const [maxLossy, setMaxLossy] = useState(
    String(settings?.compression_max_lossy_segments ?? 4),
  );
  const [ttl, setTtl] = useState(String(settings?.compression_original_ttl_secs ?? 3600));
  const [keepRatio, setKeepRatio] = useState(
    String(settings?.compression_neural_keep_ratio ?? 0.5),
  );

  function save() {
    setStatus(null);
    // Send only compression_* fields; the Management API merges partials, so the
    // other boons (vision, structured output, tool loop) are left untouched.
    const body: UpdateBoonSettings = {
      compression_enabled: enabled,
      compression_code_compaction: codeCompaction,
      compression_dedup: dedup,
      compression_compact_logs: compactLogs,
      compression_allow_lossy: allowLossy,
      compression_min_tokens: Number(minTokens) || 512,
      compression_max_segments: Number(maxSegments) || 64,
      compression_max_lossy_segments: Number(maxLossy) || 4,
      compression_original_ttl_secs: Number(ttl) || 3600,
      compression_neural_keep_ratio: Number(keepRatio) || 0.5,
    };
    start(async () => {
      const result = await setBoonSettingsAction(body);
      setStatus(
        result.ok
          ? { ok: true, message: "Compression settings saved." }
          : { ok: false, message: result.error },
      );
    });
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          <Archive className="h-4 w-4" />
          Compression
        </CardTitle>
        <CardDescription>
          Compacts long conversation history at the gateway before dispatch — lossless structural
          JSON/code compaction always, plus deterministic dedup and lossy text compaction when a
          tenant opts in via its per-tenant compression policy. Fail-open: anything that can&apos;t
          be safely shrunk passes through unchanged.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        <CompressorStatusBlock compressor={compressor} />

        <ToggleRow
          label="Enable compression boon"
          hint="Master switch. Also grant the compression boon to a model. The toggles below are system-wide defaults; a per-tenant policy overrides them."
          checked={enabled}
          onChange={() => setEnabled((value) => !value)}
        />
        <ToggleRow
          label="Code compaction by default"
          hint="Conservative whitespace stripping for fenced code blocks. A tenant policy can override this."
          checked={codeCompaction}
          onChange={() => setCodeCompaction((value) => !value)}
        />
        <ToggleRow
          label="Log template-collapse by default"
          hint="Near-lossless: repeated log lines collapse to one representative line + (×N). Best for verbose logs. A tenant policy overrides this."
          checked={compactLogs}
          onChange={() => setCompactLogs((value) => !value)}
        />
        <ToggleRow
          label="Cross-turn dedup by default"
          hint="Replace a large block repeated across messages with a [ref:HASH] marker (recoverable). A tenant policy overrides this."
          checked={dedup}
          onChange={() => setDedup((value) => !value)}
        />
        <ToggleRow
          label="Lossy text compaction by default"
          hint="Drop low-salience prose sentences (uses the neural sidecar when deployed). Lossy — original stays recoverable. A tenant policy overrides this."
          checked={allowLossy}
          onChange={() => setAllowLossy((value) => !value)}
        />

        <div className="grid gap-4 sm:grid-cols-2">
          <div className="space-y-1">
            <Label htmlFor="compression_min_tokens">Min tokens to compress</Label>
            <Input
              id="compression_min_tokens"
              type="number"
              value={minTokens}
              onChange={(e) => setMinTokens(e.target.value)}
            />
          </div>
          <div className="space-y-1">
            <Label htmlFor="compression_max_segments">Max segments</Label>
            <Input
              id="compression_max_segments"
              type="number"
              value={maxSegments}
              onChange={(e) => setMaxSegments(e.target.value)}
            />
          </div>
          <div className="space-y-1">
            <Label htmlFor="compression_max_lossy_segments">Max lossy segments</Label>
            <Input
              id="compression_max_lossy_segments"
              type="number"
              value={maxLossy}
              onChange={(e) => setMaxLossy(e.target.value)}
            />
          </div>
          <div className="space-y-1">
            <Label htmlFor="compression_original_ttl_secs">Original context TTL (secs)</Label>
            <Input
              id="compression_original_ttl_secs"
              type="number"
              value={ttl}
              onChange={(e) => setTtl(e.target.value)}
            />
          </div>
          <div className="space-y-1">
            <Label htmlFor="compression_neural_keep_ratio">Prose keep ratio</Label>
            <Input
              id="compression_neural_keep_ratio"
              type="number"
              step="0.05"
              min="0.05"
              max="1"
              value={keepRatio}
              onChange={(e) => setKeepRatio(e.target.value)}
            />
            <p className="text-xs text-muted-foreground">
              Fraction of sentences the lossy prose pass keeps (0.05–1.0). Lower is more aggressive.
              Applies to both the built-in heuristic and the neural compressor sidecar.
            </p>
          </div>
        </div>

        <div className="flex flex-wrap items-center gap-3 border-t border-border/60 pt-4">
          <Button onClick={save} disabled={pending}>
            <Save className="h-4 w-4" />
            {pending ? "Saving..." : "Save compression"}
          </Button>
          {status && (
            <p
              className={
                status.ok
                  ? "rounded-md border border-emerald-500/40 bg-emerald-500/10 px-3 py-2 text-sm text-emerald-600 dark:text-emerald-400"
                  : "rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive"
              }
            >
              {status.message}
            </p>
          )}
        </div>
      </CardContent>
    </Card>
  );
}

// `KnowledgeSettingsView` is only ever `null` here because
// `safe(obleth.getKnowledgeSettings(), null)` on the settings page swallowed a
// Management-API failure -- the Rust side always returns real defaults
// (`KnowledgeBoonSettings::default()`), never an empty/absent settings row.
// A form that doesn't know the current values must not offer to overwrite
// them: rendering the editable body with literal placeholder numbers here
// would both disagree with the actual Rust defaults (drift class already hit
// once with `MODEL_BOONS`) and let an operator unknowingly blast nine stored
// values back to those placeholders by touching one toggle. So a failed read
// gets a plain error state with a retry, never the form -- the same
// precedent `ModelKnowledgeCollectionsField` (Task 16) already set for a
// failed attachment read.
export function KnowledgeSettingsForm({ settings }: { settings: KnowledgeSettingsView | null }) {
  const router = useRouter();
  const [retrying, startRetry] = useTransition();

  if (!settings) {
    return (
      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2">
            <BookOpen className="h-4 w-4" />
            Knowledge
          </CardTitle>
        </CardHeader>
        <CardContent className="space-y-3">
          <p className="rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive">
            Could not load the current knowledge settings from the Management API. An editable form
            can&apos;t be shown without knowing the current values, since saving it would overwrite
            the stored configuration with placeholder numbers rather than what is actually set.
          </p>
          <Button
            variant="outline"
            onClick={() => startRetry(() => router.refresh())}
            disabled={retrying}
          >
            <RefreshCw className={cn("h-4 w-4", retrying && "animate-spin")} />
            {retrying ? "Retrying..." : "Retry"}
          </Button>
        </CardContent>
      </Card>
    );
  }

  return <KnowledgeSettingsFormBody settings={settings} />;
}

// Knowledge retrieval profiles: one click sets the four values that decide
// what gets injected (chunk count, score floor, token budget, query turns).
// Ingestion and operational limits are not part of a profile — they live
// under Advanced and never flip the selection to "custom".
type KnowledgeProfileValues = {
  top_k: number;
  min_score: number;
  max_context_tokens: number;
  query_turns: number;
};

const KNOWLEDGE_PROFILES: (SettingsProfile & { values: KnowledgeProfileValues })[] = [
  {
    key: "standard",
    label: "Standard",
    recommended: true,
    card: "A handful of solid hits within a modest token budget.",
    blurb:
      "The server defaults: 5 chunks per query, matches scoring under 0.35 dropped, at most 1,500 injected tokens, query built from the last 2 turns. A good fit for most collections.",
    values: { top_k: 5, min_score: 0.35, max_context_tokens: 1500, query_turns: 2 },
  },
  {
    key: "precise",
    label: "Precise",
    card: "Fewer, higher-confidence hits — clean answers over recall.",
    blurb:
      "3 chunks, only strong matches (score ≥ 0.5), an 800-token budget, query from the last turn only. Prefer when wrong context is worse than no context.",
    values: { top_k: 3, min_score: 0.5, max_context_tokens: 800, query_turns: 1 },
  },
  {
    key: "broad",
    label: "Broad",
    card: "Cast a wide net — more chunks, looser floor, bigger budget.",
    blurb:
      "10 chunks with a 0.25 score floor, up to 4,000 injected tokens, query from the last 4 turns. Prefer for exploratory questions over large collections.",
    values: { top_k: 10, min_score: 0.25, max_context_tokens: 4000, query_turns: 4 },
  },
];

function knowledgeProfileFromSettings(s: KnowledgeSettingsView): string {
  for (const p of KNOWLEDGE_PROFILES) {
    const v = p.values;
    if (
      s.top_k === v.top_k &&
      s.min_score === v.min_score &&
      s.max_context_tokens === v.max_context_tokens &&
      s.query_turns === v.query_turns
    ) {
      return p.key;
    }
  }
  return "custom";
}

// Split from `KnowledgeSettingsForm` above so its `useState` initializers only
// ever run against a real, freshly-loaded `KnowledgeSettingsView` -- this
// component only mounts once the parent has confirmed `settings` is non-null,
// including on a successful Retry (which re-mounts it with the freshly
// loaded values, rather than an already-mounted instance whose `useState`
// initializers won't re-run from new props).
function KnowledgeSettingsFormBody({ settings }: { settings: KnowledgeSettingsView }) {
  const [pending, start] = useTransition();
  const [status, setStatus] = useState<{ ok: boolean; message: string } | null>(null);

  const [enabled, setEnabled] = useState(settings.enabled);
  const [topK, setTopK] = useState(String(settings.top_k));
  const [minScore, setMinScore] = useState(String(settings.min_score));
  const [maxContextTokens, setMaxContextTokens] = useState(String(settings.max_context_tokens));
  const [embedTimeoutMs, setEmbedTimeoutMs] = useState(String(settings.embed_timeout_ms));
  const [queryCacheTtlS, setQueryCacheTtlS] = useState(String(settings.query_cache_ttl_s));
  const [queryTurns, setQueryTurns] = useState(String(settings.query_turns));
  const [maxUploadBytes, setMaxUploadBytes] = useState(String(settings.max_upload_bytes));
  const [maxChunksPerCollection, setMaxChunksPerCollection] = useState(
    String(settings.max_chunks_per_collection),
  );
  const [indexBatchSize, setIndexBatchSize] = useState(String(settings.index_batch_size));
  const [indexTimeoutMs, setIndexTimeoutMs] = useState(String(settings.index_timeout_ms));
  const [indexStaleAfterSecs, setIndexStaleAfterSecs] = useState(
    String(settings.index_stale_after_secs),
  );
  const [debugSnapshot, setDebugSnapshot] = useState(settings.debug_snapshot);
  const [profile, setProfile] = useState<string>(() => knowledgeProfileFromSettings(settings));
  const [advanced, setAdvanced] = useState(false);

  function applyProfile(key: string) {
    setProfile(key);
    const preset = KNOWLEDGE_PROFILES.find((p) => p.key === key);
    if (!preset) {
      // "Custom" keeps the current values and just opens the advanced editor.
      setAdvanced(true);
      return;
    }
    const v = preset.values;
    setTopK(String(v.top_k));
    setMinScore(String(v.min_score));
    setMaxContextTokens(String(v.max_context_tokens));
    setQueryTurns(String(v.query_turns));
  }

  // Any hand edit to a retrieval-policy value means the presets no longer
  // describe it; ingestion/operational fields deliberately don't do this.
  function custom(set: (value: string) => void): (value: string) => void {
    return (value) => {
      setProfile("custom");
      set(value);
    };
  }

  function save() {
    setStatus(null);
    // The server rejects a non-positive numeric field by silently keeping
    // the previous value rather than erroring, so `|| <fallback>` here just
    // avoids sending a bare `0` from an emptied input — it is not how an
    // operator turns anything off. `enabled` is the only off switch.
    const body: UpdateKnowledgeSettings = {
      enabled,
      top_k: Number(topK) || 1,
      min_score: Number(minScore) || 0.01,
      max_context_tokens: Number(maxContextTokens) || 1,
      embed_timeout_ms: Number(embedTimeoutMs) || 1,
      query_cache_ttl_s: Number(queryCacheTtlS) || 1,
      query_turns: Number(queryTurns) || 1,
      max_upload_bytes: Number(maxUploadBytes) || 1,
      max_chunks_per_collection: Number(maxChunksPerCollection) || 1,
      index_batch_size: Number(indexBatchSize) || 1,
      index_timeout_ms: Number(indexTimeoutMs) || 1,
      index_stale_after_secs: Number(indexStaleAfterSecs) || 1,
      debug_snapshot: debugSnapshot,
    };
    start(async () => {
      const result = await setKnowledgeSettingsAction(body);
      setStatus(
        result.ok
          ? { ok: true, message: "Knowledge settings saved." }
          : { ok: false, message: result.error },
      );
    });
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          <BookOpen className="h-4 w-4" />
          Knowledge
        </CardTitle>
        <CardDescription>
          Retrieval-augmented generation from administrator-curated collections. Grant the knowledge
          boon and attach collections on a model&apos;s edit page — retrieval is inert on a model with
          the boon granted but no collection attached.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        <ToggleRow
          label="Enable knowledge retrieval"
          hint="Master switch for the boon. When off, no request is retrieved against, regardless of per-model attachments."
          checked={enabled}
          onChange={() => setEnabled((value) => !value)}
        />

        <ProfilePicker
          label="Retrieval profile"
          profiles={KNOWLEDGE_PROFILES}
          active={profile}
          onSelect={applyProfile}
          customCard="Hand-tuned retrieval values — edit them under Advanced."
          customBlurb="The retrieval values under Advanced no longer match a preset. Picking a profile above overwrites them."
        />

        <AdvancedDisclosure
          label="Advanced — retrieval values, ingestion & operational limits"
          open={advanced}
          onToggle={() => setAdvanced((value) => !value)}
        >
          <p className="text-xs text-muted-foreground">
            Every field must be a positive number — the server silently keeps the previous value
            for zero or negative input rather than treating it as &quot;off&quot;.
          </p>

          <div className="text-sm font-medium">Retrieval</div>
          <div className="grid gap-4 sm:grid-cols-2">
            <div className="space-y-1">
              <Label htmlFor="knowledge_top_k">Top K</Label>
              <Input
                id="knowledge_top_k"
                type="number"
                min="1"
                value={topK}
                onChange={(e) => custom(setTopK)(e.target.value)}
              />
              <p className="text-xs text-muted-foreground">Chunks retrieved per query before scoring.</p>
            </div>
            <div className="space-y-1">
              <Label htmlFor="knowledge_min_score">Minimum score</Label>
              <Input
                id="knowledge_min_score"
                type="number"
                step="0.01"
                min="0.01"
                max="1"
                value={minScore}
                onChange={(e) => custom(setMinScore)(e.target.value)}
              />
              <p className="text-xs text-muted-foreground">Hits scoring below this are dropped before injection.</p>
            </div>
            <div className="space-y-1">
              <Label htmlFor="knowledge_max_context_tokens">Max context tokens</Label>
              <Input
                id="knowledge_max_context_tokens"
                type="number"
                min="1"
                value={maxContextTokens}
                onChange={(e) => custom(setMaxContextTokens)(e.target.value)}
              />
              <p className="text-xs text-muted-foreground">Token budget for injected retrieval per request.</p>
            </div>
            <div className="space-y-1">
              <Label htmlFor="knowledge_query_turns">Query turns</Label>
              <Input
                id="knowledge_query_turns"
                type="number"
                min="1"
                value={queryTurns}
                onChange={(e) => custom(setQueryTurns)(e.target.value)}
              />
              <p className="text-xs text-muted-foreground">Trailing conversation turns folded into the retrieval query.</p>
            </div>
          </div>

          <div className="border-t border-border/60 pt-3 text-sm font-medium">
            Ingestion &amp; operations
          </div>
          <p className="-mt-2 text-xs text-muted-foreground">
            Plumbing limits, not retrieval policy — changing these never leaves the profile above.
          </p>
          <div className="grid gap-4 sm:grid-cols-2">
            <div className="space-y-1">
              <Label htmlFor="knowledge_embed_timeout_ms">Embed timeout (ms)</Label>
              <Input
                id="knowledge_embed_timeout_ms"
                type="number"
                min="1"
                value={embedTimeoutMs}
                onChange={(e) => setEmbedTimeoutMs(e.target.value)}
              />
              <p className="text-xs text-muted-foreground">
                Query-embedding call budget. On timeout the request proceeds ungrounded rather than
                failing.
              </p>
            </div>
            <div className="space-y-1">
              <Label htmlFor="knowledge_query_cache_ttl_s">Query cache TTL (secs)</Label>
              <Input
                id="knowledge_query_cache_ttl_s"
                type="number"
                min="1"
                value={queryCacheTtlS}
                onChange={(e) => setQueryCacheTtlS(e.target.value)}
              />
              <p className="text-xs text-muted-foreground">
                How long a query&apos;s embedding vector is cached, keyed per embedder.
              </p>
            </div>
            <div className="space-y-1">
              <Label htmlFor="knowledge_max_upload_bytes">Max upload size</Label>
              <Input
                id="knowledge_max_upload_bytes"
                type="number"
                min="1"
                value={maxUploadBytes}
                onChange={(e) => setMaxUploadBytes(e.target.value)}
              />
              <p className="text-xs text-muted-foreground">
                {formatBytes(Number(maxUploadBytes) || 0)} per document upload.
              </p>
            </div>
            <div className="space-y-1">
              <Label htmlFor="knowledge_max_chunks_per_collection">Max chunks per collection</Label>
              <Input
                id="knowledge_max_chunks_per_collection"
                type="number"
                min="1"
                value={maxChunksPerCollection}
                onChange={(e) => setMaxChunksPerCollection(e.target.value)}
              />
              <p className="text-xs text-muted-foreground">Indexing stops adding chunks once a collection reaches this cap.</p>
            </div>
            <div className="space-y-1">
              <Label htmlFor="knowledge_index_batch_size">Index batch size</Label>
              <Input
                id="knowledge_index_batch_size"
                type="number"
                min="1"
                value={indexBatchSize}
                onChange={(e) => setIndexBatchSize(e.target.value)}
              />
              <p className="text-xs text-muted-foreground">Chunks embedded per indexer batch.</p>
            </div>
            <div className="space-y-1">
              <Label htmlFor="knowledge_index_timeout_ms">Index timeout (ms)</Label>
              <Input
                id="knowledge_index_timeout_ms"
                type="number"
                min="1"
                value={indexTimeoutMs}
                onChange={(e) => setIndexTimeoutMs(e.target.value)}
              />
              <p className="text-xs text-muted-foreground">Per-batch embedding call budget during background indexing.</p>
            </div>
            <div className="space-y-1">
              <Label htmlFor="knowledge_index_stale_after_secs">Stale after (secs)</Label>
              <Input
                id="knowledge_index_stale_after_secs"
                type="number"
                min="1"
                value={indexStaleAfterSecs}
                onChange={(e) => setIndexStaleAfterSecs(e.target.value)}
              />
              <p className="text-xs text-muted-foreground">
                A document stuck indexing past this age is treated as failed and eligible for retry.
              </p>
            </div>
          </div>
        </AdvancedDisclosure>

        <div className="flex items-start justify-between gap-4 rounded-lg border border-amber-500/40 bg-amber-500/10 px-4 py-3">
          <div className="min-w-0">
            <p className="text-sm font-medium">Debug snapshot</p>
            <p className="mt-0.5 text-[11px] leading-snug text-amber-600 dark:text-amber-300">
              Writes retrieved chunk text into trace spans, at roughly 20&times; the storage of the
              default tracing tier. Enable only for a short debugging session, then turn it back off.
            </p>
          </div>
          <ToggleSwitch checked={debugSnapshot} onChange={() => setDebugSnapshot((value) => !value)} label="Debug snapshot" />
        </div>

        <div className="flex flex-wrap items-center gap-3 border-t border-border/60 pt-4">
          <Button onClick={save} disabled={pending}>
            <Save className="h-4 w-4" />
            {pending ? "Saving..." : "Save knowledge settings"}
          </Button>
          {status && (
            <p
              className={
                status.ok
                  ? "rounded-md border border-emerald-500/40 bg-emerald-500/10 px-3 py-2 text-sm text-emerald-600 dark:text-emerald-400"
                  : "rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive"
              }
            >
              {status.message}
            </p>
          )}
        </div>
      </CardContent>
    </Card>
  );
}

export function CharoSettingsForm({
  settings,
  models,
}: {
  settings: CharoSettingsView | null;
  models: ModelRoute[];
}) {
  const [pending, start] = useTransition();
  const [status, setStatus] = useState<{ ok: boolean; message: string } | null>(null);
  const [enabled, setEnabled] = useState(settings?.enabled ?? true);
  const [brain, setBrain] = useState<string>(settings?.brain_model ?? "");
  const [maxConc, setMaxConc] = useState(settings?.bench_max_concurrency ?? 40);
  const [maxDur, setMaxDur] = useState(settings?.bench_max_duration_s ?? 120);
  const [maxReq, setMaxReq] = useState(settings?.bench_max_requests ?? 500);
  const [runBench, setRunBench] = useState(settings?.tools_enabled?.run_benchmark ?? true);

  const brainCandidates = models.filter((m) => m.enabled && m.supports_function_calling);

  function save() {
    setStatus(null);
    start(async () => {
      const next: CharoSettingsView = {
        enabled,
        brain_model: brain || null,
        tools_enabled: { run_benchmark: runBench },
        bench_max_concurrency: maxConc,
        bench_max_duration_s: maxDur,
        bench_max_requests: maxReq,
      };
      const result = await setCharoSettingsAction(next);
      setStatus(
        result.ok
          ? { ok: true, message: "Assistant settings saved." }
          : { ok: false, message: result.error },
      );
    });
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle>Playground assistant</CardTitle>
        <CardDescription>
          The dashboard&apos;s built-in assistant, available in Playground. Give it an agent
          model to let it run tools (like the capacity benchmark) and answer with live results.
          Without one it acts as a plain model tester: no tools, and its persona rides the model
          under test. Every token is billed to the reserved internal tenant.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-5">
        <ToggleRow
          label="Enable Playground in the dashboard"
          hint="Off hides the Playground page and the assistant launcher entirely."
          checked={enabled}
          onChange={() => setEnabled((value) => !value)}
        />

        <div className="space-y-1.5">
          <label className="text-sm font-medium">Agent model</label>
          <Select
            aria-label="Agent model"
            value={brain}
            onValueChange={setBrain}
            searchPlaceholder="Filter models"
            options={[
              { value: "", label: "None (plain model tester)" },
              ...brainCandidates.map((m) => ({ value: m.model_name, label: m.model_name })),
            ]}
          />
          <p className="text-xs text-muted-foreground">
            Runs the assistant&apos;s tool loop, so only function-calling models qualify.{" "}
            {brainCandidates.length === 0 && "No enabled model supports function calling yet."}
          </p>
        </div>

        <div className="space-y-2">
          <div className="text-sm font-medium">Tools</div>
          <label className="flex items-center gap-2 text-sm">
            <Checkbox checked={runBench} onChange={setRunBench} />
            Capacity benchmark (<code>run_benchmark</code>)
          </label>
        </div>

        <div className="space-y-1.5">
          <div className="text-sm font-medium">Benchmark limits</div>
          <p className="text-xs text-muted-foreground">
            Ceilings for benchmarks the assistant starts, so a casual ask can&apos;t swamp the
            fleet.
          </p>
          <div className="grid grid-cols-3 gap-3">
            <div className="space-y-1">
              <Label htmlFor="assistant_bench_max_concurrency">Max concurrency</Label>
              <Input
                id="assistant_bench_max_concurrency"
                type="number"
                min={1}
                value={maxConc}
                onChange={(e) => setMaxConc(Number(e.target.value))}
              />
            </div>
            <div className="space-y-1">
              <Label htmlFor="assistant_bench_max_duration_s">Max duration (s)</Label>
              <Input
                id="assistant_bench_max_duration_s"
                type="number"
                min={1}
                value={maxDur}
                onChange={(e) => setMaxDur(Number(e.target.value))}
              />
            </div>
            <div className="space-y-1">
              <Label htmlFor="assistant_bench_max_requests">Max requests</Label>
              <Input
                id="assistant_bench_max_requests"
                type="number"
                min={1}
                value={maxReq}
                onChange={(e) => setMaxReq(Number(e.target.value))}
              />
            </div>
          </div>
        </div>

        <Button onClick={save} disabled={pending}>{pending ? "Saving..." : "Save assistant"}</Button>
        {status && (
          <p className={status.ok
            ? "rounded-md border border-emerald-500/40 bg-emerald-500/10 px-3 py-2 text-sm text-emerald-600 dark:text-emerald-400"
            : "rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive"}>
            {status.message}
          </p>
        )}
      </CardContent>
    </Card>
  );
}

/** Human-readable "time ago" for the provisioner's last-seen heartbeat. */
function formatLastSeen(secs: number | null): string {
  if (secs == null) return "never";
  if (secs < 0) return "just now";
  if (secs < 60) return `${secs}s ago`;
  if (secs < 3600) return `${Math.floor(secs / 60)}m ago`;
  return `${Math.floor(secs / 3600)}h ago`;
}

export function SlurmSettingsForm({ settings }: { settings: SlurmSettingsView | null }) {
  const [pending, start] = useTransition();
  const [testing, startTest] = useTransition();
  const [status, setStatus] = useState<{ ok: boolean; message: string } | null>(null);
  const [health, setHealth] = useState<SlurmHealthView | null>(null);

  const [enabled, setEnabled] = useState(settings?.enabled ?? false);
  const [url, setUrl] = useState(settings?.slurmrestd_url ?? "");
  const [version, setVersion] = useState(settings?.slurmrestd_api_version ?? "v0.0.40");
  const [user, setUser] = useState(settings?.slurm_user ?? "");
  const jwtSet = settings?.jwt_set ?? false;
  const jwtLast4 = settings?.jwt_last4 ?? null;
  const [jwt, setJwt] = useState("");
  const [aliases, setAliases] = useState<NodeAlias[]>(settings?.node_aliases ?? []);

  function save() {
    setStatus(null);
    setHealth(null);
    const body: UpdateSlurmSettings = {
      enabled,
      slurmrestd_url: url.trim(),
      slurmrestd_api_version: version.trim() || "v0.0.40",
      slurm_user: user.trim(),
      node_aliases: aliases
        .map((a) => ({ host: a.host.trim(), ip: a.ip.trim() }))
        .filter((a) => a.host || a.ip),
    };
    if (jwt.trim()) body.slurm_jwt = jwt.trim();
    start(async () => {
      const result = await setSlurmSettingsAction(body);
      if (result.ok) {
        setStatus({ ok: true, message: "Slurm settings saved." });
        setJwt("");
      } else {
        setStatus({ ok: false, message: result.error });
      }
    });
  }

  function test() {
    setStatus(null);
    setHealth(null);
    startTest(async () => {
      const result = await testSlurmConnectionAction();
      if (result.ok) {
        setHealth(result.health ?? null);
      } else {
        setStatus({ ok: false, message: result.error });
      }
    });
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          <Server className="h-4 w-4" />
          Slurm provisioning
        </CardTitle>
        <CardDescription>
          Connection details for the optional <strong>obleth-provisioner</strong> plugin, which
          keeps Slurm-hosted models alive on a preemptible cluster via <code>slurmrestd</code>.
          When enabled, models created with a <strong>Slurm</strong> endpoint are provisioned
          automatically. The JWT is stored encrypted at rest and never shown again after saving.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        <label className="flex items-center gap-2 text-sm">
          <Checkbox checked={enabled} onChange={setEnabled} />
          Enable Slurm provisioning
        </label>
        {!enabled && settings?.enabled && (
          <p className="rounded-md border border-amber-500/40 bg-amber-500/10 px-3 py-2 text-xs text-amber-600 dark:text-amber-400">
            Disabling stops reconciliation but does <strong>not</strong> cancel Slurm jobs that are
            already running — they keep their allocations, and their replica entries freeze at
            their current state. To stop hosting a model and release its nodes, disable that model
            (or set its target replicas to 0) before turning Slurm off here.
          </p>
        )}

        {settings?.enabled && (() => {
          // Three states, not two: alive + reconciling (green), alive but the
          // reconcile tick keeps failing/holding (amber — replica state across
          // the dashboard is frozen), and process not detected at all (amber).
          const reconcileFailing =
            settings.provisioner_running &&
            settings.provisioner_tick_status != null &&
            settings.provisioner_tick_status !== "ok" &&
            (settings.provisioner_held_secs ?? 0) > 60;
          const healthyBox = settings.provisioner_running && !reconcileFailing;
          return (
            <div
              className={
                healthyBox
                  ? "flex items-start gap-2 rounded-md border border-emerald-500/40 bg-emerald-500/10 px-3 py-2 text-sm text-emerald-600 dark:text-emerald-400"
                  : "flex items-start gap-2 rounded-md border border-amber-500/40 bg-amber-500/10 px-3 py-2 text-sm text-amber-600 dark:text-amber-400"
              }
            >
              <span
                className={
                  healthyBox
                    ? "mt-1 inline-block h-2 w-2 shrink-0 rounded-full bg-emerald-500"
                    : "mt-1 inline-block h-2 w-2 shrink-0 rounded-full bg-amber-500"
                }
              />
              <span>
                {healthyBox ? (
                  <>
                    <span className="font-medium">Provisioner running</span> — last polled{" "}
                    {formatLastSeen(settings.provisioner_last_seen_secs)}.
                    {settings.provisioner_version && (
                      <>
                        {" "}
                        <span className="font-mono">
                          v{settings.provisioner_version}
                          {settings.provisioner_git_sha && (
                            <span className="ml-1 text-xs opacity-70">
                              {settings.provisioner_git_sha.slice(0, 7)}
                            </span>
                          )}
                        </span>
                      </>
                    )}
                  </>
                ) : reconcileFailing ? (
                  <>
                    <span className="font-medium">Provisioner running, but reconciliation is failing</span>{" "}
                    ({formatLastSeen(settings.provisioner_held_secs).replace(" ago", "")} and counting;
                    last successful tick {formatLastSeen(settings.provisioner_last_ok_secs)}).
                    {settings.provisioner_tick_detail && (
                      <> Last error: <span className="font-mono text-xs">{settings.provisioner_tick_detail}</span>.</>
                    )}{" "}
                    Replica states shown on model pages are frozen until this clears — check that{" "}
                    <code>slurmrestd</code> is reachable (Test connection below) and the JWT is valid.
                  </>
                ) : (
                  <>
                    <span className="font-medium">Provisioner not detected</span>{" "}
                    (last polled {formatLastSeen(settings.provisioner_last_seen_secs)}). Enabling
                    Slurm only stores these connection details — the separate{" "}
                    <code>obleth-provisioner</code> process must be running for replicas to launch.
                    In Kubernetes set <code>provisioner.enabled=true</code>; in Docker add{" "}
                    <code>slurm</code> to <code>COMPOSE_PROFILES</code>.
                  </>
                )}
              </span>
            </div>
          );
        })()}

        <div className="grid gap-4 sm:grid-cols-2">
          <div className="space-y-1.5">
            <Label htmlFor="slurmrestd_url">slurmrestd URL</Label>
            <Input
              id="slurmrestd_url"
              type="url"
              value={url}
              onChange={(e) => setUrl(e.target.value)}
              placeholder="http://slurm:6820"
            />
          </div>
          <div className="space-y-1.5">
            <Label htmlFor="slurmrestd_api_version">API version</Label>
            <Input
              id="slurmrestd_api_version"
              value={version}
              onChange={(e) => setVersion(e.target.value)}
              placeholder="v0.0.40"
            />
          </div>
          <div className="space-y-1.5">
            <Label htmlFor="slurm_user">Slurm user</Label>
            <Input
              id="slurm_user"
              value={user}
              onChange={(e) => setUser(e.target.value)}
              placeholder="obleth"
              autoComplete="off"
            />
          </div>
          <div className="space-y-1.5">
            <Label htmlFor="slurm_jwt">Slurm JWT</Label>
            <Input
              id="slurm_jwt"
              type="password"
              value={jwt}
              onChange={(e) => setJwt(e.target.value)}
              placeholder={
                jwtSet
                  ? `•••• ${jwtLast4 ?? ""} (configured — leave blank to keep)`
                  : "paste the slurmrestd JWT"
              }
              autoComplete="new-password"
            />
          </div>
        </div>

        <div className="space-y-2 rounded-md border border-border bg-muted/20 p-3">
          <div className="space-y-0.5">
            <Label>Node address overrides</Label>
            <p className="text-xs text-muted-foreground">
              Map Slurm node hostnames to IPs for clusters where the pods running obleth resolve
              node names unreliably. The provisioner then registers each replica&apos;s endpoint by
              IP and probes by IP, so neither health checks nor proxied requests depend on
              per-request DNS. Leave empty to resolve node names through DNS.
            </p>
          </div>
          {aliases.length > 0 && (
            <div className="space-y-2">
              {aliases.map((a, i) => (
                <div key={i} className="flex items-center gap-2">
                  <Input
                    aria-label="Node hostname"
                    value={a.host}
                    onChange={(e) =>
                      setAliases((prev) =>
                        prev.map((x, j) => (j === i ? { ...x, host: e.target.value } : x)),
                      )
                    }
                    placeholder="node001"
                    className="font-mono"
                    autoComplete="off"
                  />
                  <span className="shrink-0 text-muted-foreground">→</span>
                  <Input
                    aria-label="IP address"
                    value={a.ip}
                    onChange={(e) =>
                      setAliases((prev) =>
                        prev.map((x, j) => (j === i ? { ...x, ip: e.target.value } : x)),
                      )
                    }
                    placeholder="10.139.125.25"
                    className="font-mono"
                    autoComplete="off"
                  />
                  <Button
                    type="button"
                    variant="ghost"
                    size="sm"
                    onClick={() => setAliases((prev) => prev.filter((_, j) => j !== i))}
                    aria-label="Remove override"
                  >
                    Remove
                  </Button>
                </div>
              ))}
            </div>
          )}
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={() => setAliases((prev) => [...prev, { host: "", ip: "" }])}
          >
            Add node override
          </Button>
        </div>

        <div className="flex flex-wrap items-center gap-3">
          <Button onClick={save} disabled={pending}>
            {pending ? "Saving…" : "Save settings"}
          </Button>
          <Button variant="outline" onClick={test} disabled={testing}>
            <Send className="mr-2 h-4 w-4" />
            {testing ? "Testing…" : "Test connection"}
          </Button>
          <span className="text-xs text-muted-foreground">
            Test uses the saved settings — save first if you just made changes.
          </span>
        </div>

        {status && (
          <p
            className={
              status.ok
                ? "rounded-md border border-emerald-500/40 bg-emerald-500/10 px-3 py-2 text-sm text-emerald-600 dark:text-emerald-400"
                : "rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive"
            }
          >
            {status.message}
          </p>
        )}

        {health && (
          <div className="space-y-2 rounded-md border border-border bg-muted/30 p-3 text-sm">
            <div className="flex items-start gap-2">
              <span
                className={
                  health.ping.ok
                    ? "mt-0.5 inline-block h-2 w-2 shrink-0 rounded-full bg-emerald-500"
                    : "mt-0.5 inline-block h-2 w-2 shrink-0 rounded-full bg-destructive"
                }
              />
              <span className="font-medium">slurmrestd ping:</span>
              <span className="text-muted-foreground">
                {health.ping.ok
                  ? `OK (${health.ping.status_code}, ${health.ping.latency_ms}ms)`
                  : health.ping.error ?? `failed (${health.ping.status_code ?? "no response"})`}
              </span>
            </div>
            <div className="flex items-start gap-2">
              <span
                className={
                  health.jwt.set && !health.jwt.expired
                    ? "mt-0.5 inline-block h-2 w-2 shrink-0 rounded-full bg-emerald-500"
                    : "mt-0.5 inline-block h-2 w-2 shrink-0 rounded-full bg-destructive"
                }
              />
              <span className="font-medium">JWT:</span>
              <span className="text-muted-foreground">
                {!health.jwt.set
                  ? "not configured"
                  : health.jwt.expired
                    ? `expired${health.jwt.expires_at ? ` ${new Date(health.jwt.expires_at).toLocaleString()}` : ""}`
                    : health.jwt.expires_at
                      ? `valid — expires ${new Date(health.jwt.expires_at).toLocaleString()}`
                      : "valid (no expiry claim)"}
              </span>
            </div>
          </div>
        )}
      </CardContent>
    </Card>
  );
}

export function UsageRetentionForm({ retention }: { retention: UsageRetentionView | null }) {
  const currentDays = retention?.days ?? 180;
  const [pending, start] = useTransition();
  const [compacting, startCompact] = useTransition();
  const [selected, setSelected] = useState<number>(currentDays);
  const [status, setStatus] = useState<{ ok: boolean; message: string } | null>(null);
  const [confirmLower, setConfirmLower] = useState(false);
  const [confirmCompact, setConfirmCompact] = useState(false);

  // Offer the saved value even if it isn't one of the presets.
  const options = Array.from(new Set([...RETENTION_PRESETS, currentDays])).sort((a, b) => a - b);
  const lowering = selected < currentDays;

  function persist(days: number) {
    setStatus(null);
    start(async () => {
      const result = await setUsageRetentionAction(days);
      setStatus(
        result.ok
          ? { ok: true, message: `Retention set to ${days} days.` }
          : { ok: false, message: result.error },
      );
      setConfirmLower(false);
    });
  }

  function onSave() {
    if (lowering) {
      setConfirmLower(true);
    } else {
      persist(selected);
    }
  }

  function onCompact() {
    setStatus(null);
    startCompact(async () => {
      const result = await compactUsageAction();
      if (result.ok) {
        setStatus({
          ok: true,
          message: `Compacted: dropped ${result.partitionsDropped ?? 0} day-partition(s) older than ${result.retentionDays ?? currentDays} days.`,
        });
      } else {
        setStatus({ ok: false, message: result.error });
      }
      setConfirmCompact(false);
    });
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          <Database className="h-4 w-4" />
          Usage data retention
        </CardTitle>
        <CardDescription>
          Raw per-request usage rows are kept for this many days, then pruned to bound storage. The
          permanent daily rollup powering the Reports page is <strong>kept forever</strong> and is
          never affected by this setting.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        <div className="grid gap-4 sm:grid-cols-2">
          <div className="space-y-1">
            <Label htmlFor="retention_days">Retention window</Label>
            <Select
              id="retention_days"
              value={String(selected)}
              onValueChange={(value) => setSelected(Number(value))}
              options={options.map((d) => ({
                value: String(d),
                label: `${d} days${d === currentDays ? " (current)" : ""}`,
              }))}
            />
          </div>
        </div>

        <div className="flex flex-wrap gap-2">
          <Button onClick={onSave} disabled={pending || selected === currentDays}>
            {pending ? "Saving..." : "Save retention"}
          </Button>
          <Button variant="destructive" onClick={() => setConfirmCompact(true)} disabled={compacting}>
            <Trash2 className="h-4 w-4" />
            {compacting ? "Compacting..." : "Compact now"}
          </Button>
        </div>

        {status && (
          <p
            className={
              status.ok
                ? "rounded-md border border-emerald-500/40 bg-emerald-500/10 px-3 py-2 text-sm text-emerald-600 dark:text-emerald-400"
                : "rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive"
            }
          >
            {status.message}
          </p>
        )}
      </CardContent>

      <DestructiveConfirm
        open={confirmLower}
        onOpenChange={setConfirmLower}
        title="Lower usage retention"
        checkboxLabel="I understand raw per-request data older than the new window will be permanently deleted."
        confirmLabel={`Lower to ${selected} days`}
        pending={pending}
        onConfirm={() => persist(selected)}
        description={
          <>
            <p>
              Lowering retention from <strong>{currentDays}</strong> to{" "}
              <strong>{selected}</strong> days will, on the next compaction, permanently delete raw
              per-request rows older than {selected} days.
            </p>
            <p>
              Daily totals on the Reports page are <strong>not</strong> affected. This cannot be
              undone.
            </p>
          </>
        }
      />

      <DestructiveConfirm
        open={confirmCompact}
        onOpenChange={setConfirmCompact}
        title="Compact usage data now"
        checkboxLabel="I understand this immediately deletes raw per-request data outside the retention window."
        confirmLabel="Compact now"
        pending={compacting}
        onConfirm={onCompact}
        description={
          <>
            <p>
              This immediately drops every raw <code>usage</code> day-partition older than the
              current retention window ({currentDays} days), reclaiming storage.
            </p>
            <p>
              The permanent daily rollup is <strong>not</strong> touched. This cannot be undone.
            </p>
          </>
        }
      />
    </Card>
  );
}

function plural(n: number, noun: string): string {
  return `${n} ${noun}${n === 1 ? "" : "s"}`;
}

/** Operator-facing summary of a resolver-cache reconcile. */
export function describeResync(report: ResyncReport): string {
  const republished = `Republished ${plural(report.keys, "key")}, ${plural(report.models, "model")}, and ${plural(report.mcp_servers, "MCP server")}.`;
  const evicted = report.keys_pruned + report.model_names_pruned + report.mcp_servers_pruned;
  if (evicted === 0) return `${republished} No stale entries found.`;
  return `${republished} Evicted ${plural(report.keys_pruned, "stale key")}, ${plural(report.model_names_pruned, "stale model name")}, and ${plural(report.mcp_servers_pruned, "stale MCP server")}.`;
}

export function ResolverCacheCard() {
  const [pending, start] = useTransition();
  const [status, setStatus] = useState<{ ok: boolean; message: string } | null>(null);

  function onReconcile() {
    setStatus(null);
    start(async () => {
      const result = await resyncCacheAction();
      setStatus(
        result.ok && result.report
          ? { ok: true, message: describeResync(result.report) }
          : { ok: false, message: result.ok ? "No report returned" : result.error },
      );
    });
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          <RefreshCw className="h-4 w-4" />
          Resolver cache
        </CardTitle>
        <CardDescription>
          Gateways resolve API keys, models, and MCP servers from a Redis cache kept in step with the
          database. Reconciling republishes every entry from the database and evicts entries that no
          longer have a backing record. Use it when a delete reports that the cache eviction failed.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        <Button onClick={onReconcile} disabled={pending}>
          <RefreshCw className={cn("h-4 w-4", pending && "animate-spin")} />
          {pending ? "Reconciling..." : "Reconcile cache"}
        </Button>
        {status && (
          <p
            className={
              status.ok
                ? "rounded-md border border-emerald-500/40 bg-emerald-500/10 px-3 py-2 text-sm text-emerald-600 dark:text-emerald-400"
                : "rounded-md border border-destructive/40 bg-destructive/10 px-3 py-2 text-sm text-destructive"
            }
          >
            {status.message}
          </p>
        )}
      </CardContent>
    </Card>
  );
}
