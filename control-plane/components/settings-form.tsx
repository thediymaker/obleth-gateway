"use client";

import { useState, useTransition } from "react";
import { Send, Database, Trash2 } from "lucide-react";
import {
  setAlertSettingsAction,
  setAutoRouterSettingsAction,
  testAlertAction,
  setUsageRetentionAction,
  compactUsageAction,
} from "@/app/actions";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { DestructiveConfirm } from "@/components/ui/destructive-confirm";
import type {
  AlertSettingsView,
  AutoRouterSettingsView,
  ModelRoute,
  UpdateAlertSettings,
  UpdateAutoRouterSettings,
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
