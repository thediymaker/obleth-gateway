"use client";

import { useState, useTransition, type ReactNode } from "react";
import {
  Archive,
  Braces,
  ChevronDown,
  Database,
  Eye,
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
  testAlertAction,
  setSlurmSettingsAction,
  testSlurmConnectionAction,
  setUsageRetentionAction,
  compactUsageAction,
} from "@/app/actions";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/badge";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { DestructiveConfirm } from "@/components/ui/destructive-confirm";
import { cn } from "@/lib/utils";
import type {
  AlertSettingsView,
  AutoRouterSettingsView,
  BoonSettingsView,
  CharoSettingsView,
  CompressorStatusView,
  ModelRoute,
  SlurmHealthView,
  SlurmSettingsView,
  UpdateAlertSettings,
  UpdateAutoRouterSettings,
  UpdateBoonSettings,
  UpdateSlurmSettings,
  UsageRetentionView,
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
              <input
                type="checkbox"
                checked={clearSlack}
                onChange={(e) => setClearSlack(e.target.checked)}
                className="h-4 w-4 rounded border-border"
              />
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
            <input
              type="checkbox"
              checked={emailEnabled}
              onChange={(e) => setEmailEnabled(e.target.checked)}
              className="h-4 w-4 rounded border-border"
            />
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
                    <input
                      type="checkbox"
                      checked={clearPassword}
                      onChange={(e) => setClearPassword(e.target.checked)}
                      className="h-3.5 w-3.5 rounded border-border"
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
                  <input
                    type="checkbox"
                    checked={starttls}
                    onChange={(e) => setStarttls(e.target.checked)}
                    className="h-4 w-4 rounded border-border"
                  />
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

  function save() {
    setStatus(null);
    const body: UpdateAutoRouterSettings = {
      classifier_enabled: enabled,
      classifier_model: model.trim() ? model.trim() : "",
      classifier_timeout_ms: Number(timeout) || 250,
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
        <label className="flex items-center gap-2 text-sm">
          <input
            type="checkbox"
            checked={enabled}
            onChange={(e) => setEnabled(e.target.checked)}
            className="h-4 w-4"
          />
          Enable intent classifier
        </label>
        <div className="grid gap-4 sm:grid-cols-2">
          <div className="space-y-1">
            <Label htmlFor="classifier_model">Classifier model</Label>
            <select
              id="classifier_model"
              value={model}
              onChange={(e) => setModel(e.target.value)}
              className="flex h-9 w-full rounded-md border border-input bg-background px-3 py-1 text-sm"
            >
              <option value="">None</option>
              {models
                .filter((m) => m.model_name !== "auto")
                .map((m) => (
                  <option key={m.id} value={m.model_name}>
                    {m.model_name}
                  </option>
                ))}
            </select>
          </div>
          <div className="space-y-1">
            <Label htmlFor="classifier_timeout_ms">Timeout (ms)</Label>
            <Input
              id="classifier_timeout_ms"
              type="number"
              value={timeout}
              onChange={(e) => setTimeoutMs(e.target.value)}
            />
          </div>
        </div>
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

type BoonSectionKey = "vision" | "structured" | "tool_loop";

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
  const [expanded, setExpanded] = useState<BoonSectionKey | null>(null);

  const visionModels = models.filter(
    (m) => m.model_name !== "auto" && (m.supports_vision || (m.tags?.includes("vision") ?? false)),
  );
  const chatModels = models.filter(
    (m) => m.model_name !== "auto" && (m.model_type ?? "chat") === "chat",
  );

  function toggleSection(section: BoonSectionKey) {
    setExpanded((current) => (current === section ? null : section));
  }

  function save() {
    setStatus(null);
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
                  <select
                    id="vision_fallback_model"
                    value={model}
                    onChange={(e) => setModel(e.target.value)}
                    className="flex h-9 w-full rounded-md border border-input bg-background px-3 py-1 text-sm"
                  >
                    <option value="">None</option>
                    {visionModels.map((m) => (
                      <option key={m.id} value={m.model_name}>
                        {m.model_name}
                      </option>
                    ))}
                  </select>
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
                  <select
                    id="structured_output_fixer_model"
                    value={fixerModel}
                    onChange={(e) => setFixerModel(e.target.value)}
                    className="flex h-9 w-full rounded-md border border-input bg-background px-3 py-1 text-sm"
                  >
                    <option value="">Same model (re-prompt)</option>
                    {chatModels.map((m) => (
                      <option key={m.id} value={m.model_name}>
                        {m.model_name}
                      </option>
                    ))}
                  </select>
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
        <CardTitle>Charo assistant</CardTitle>
        <CardDescription>
          Charo is an on-screen operator companion. Give it a brain model to let it run
          tools (like the capacity benchmark) and answer with live results. Without a brain
          model it stays a plain model-tester: the persona rides the model under test and no
          tools are offered. Every token is billed to the reserved internal tenant.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-5">
        <label className="flex items-center gap-2 text-sm">
          <input type="checkbox" checked={enabled} onChange={(e) => setEnabled(e.target.checked)} className="h-4 w-4" />
          Show Charo in the dashboard
        </label>

        <div className="space-y-1.5">
          <label className="text-sm font-medium">Brain model</label>
          <select
            value={brain}
            onChange={(e) => setBrain(e.target.value)}
            className="h-9 w-full rounded-md border border-border bg-background px-3 text-sm"
          >
            <option value="">None (legacy tester mode)</option>
            {brainCandidates.map((m) => (
              <option key={m.id} value={m.model_name}>{m.model_name}</option>
            ))}
          </select>
          <p className="text-xs text-muted-foreground">
            Only function-calling models can be a brain. {brainCandidates.length === 0 && "No enabled model supports function calling yet."}
          </p>
        </div>

        <div className="space-y-2">
          <div className="text-sm font-medium">Tools</div>
          <label className="flex items-center gap-2 text-sm">
            <input type="checkbox" checked={runBench} onChange={(e) => setRunBench(e.target.checked)} className="h-4 w-4" />
            Capacity benchmark (<code>run_benchmark</code>)
          </label>
        </div>

        <div className="grid grid-cols-3 gap-3">
          <label className="space-y-1 text-sm">
            <span className="text-muted-foreground">Max concurrency</span>
            <input type="number" min={1} value={maxConc} onChange={(e) => setMaxConc(Number(e.target.value))}
              className="h-9 w-full rounded-md border border-border bg-background px-3 text-sm" />
          </label>
          <label className="space-y-1 text-sm">
            <span className="text-muted-foreground">Max duration (s)</span>
            <input type="number" min={1} value={maxDur} onChange={(e) => setMaxDur(Number(e.target.value))}
              className="h-9 w-full rounded-md border border-border bg-background px-3 text-sm" />
          </label>
          <label className="space-y-1 text-sm">
            <span className="text-muted-foreground">Max requests</span>
            <input type="number" min={1} value={maxReq} onChange={(e) => setMaxReq(Number(e.target.value))}
              className="h-9 w-full rounded-md border border-border bg-background px-3 text-sm" />
          </label>
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

  function save() {
    setStatus(null);
    setHealth(null);
    const body: UpdateSlurmSettings = {
      enabled,
      slurmrestd_url: url.trim(),
      slurmrestd_api_version: version.trim() || "v0.0.40",
      slurm_user: user.trim(),
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
          <input
            type="checkbox"
            checked={enabled}
            onChange={(e) => setEnabled(e.target.checked)}
            className="h-4 w-4 rounded border-border"
          />
          Enable Slurm provisioning
        </label>

        {settings?.enabled && (
          <div
            className={
              settings.provisioner_running
                ? "flex items-start gap-2 rounded-md border border-emerald-500/40 bg-emerald-500/10 px-3 py-2 text-sm text-emerald-600 dark:text-emerald-400"
                : "flex items-start gap-2 rounded-md border border-amber-500/40 bg-amber-500/10 px-3 py-2 text-sm text-amber-600 dark:text-amber-400"
            }
          >
            <span
              className={
                settings.provisioner_running
                  ? "mt-1 inline-block h-2 w-2 shrink-0 rounded-full bg-emerald-500"
                  : "mt-1 inline-block h-2 w-2 shrink-0 rounded-full bg-amber-500"
              }
            />
            <span>
              {settings.provisioner_running ? (
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
        )}

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
            <select
              id="retention_days"
              value={selected}
              onChange={(e) => setSelected(Number(e.target.value))}
              className="flex h-9 w-full rounded-md border border-input bg-background px-3 py-1 text-sm"
            >
              {options.map((d) => (
                <option key={d} value={d}>
                  {d} days{d === currentDays ? " (current)" : ""}
                </option>
              ))}
            </select>
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
