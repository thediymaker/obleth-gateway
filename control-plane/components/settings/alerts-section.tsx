"use client";

import { useState, useTransition } from "react";
import { testAlertAction } from "@/app/actions";
import { SettingsCard } from "@/components/access/ui";
import { Glyph } from "@/components/deployments/ui";
import { Setting, Switch, TextArea, TextField } from "@/components/models/fields";
import { Pill } from "@/components/overview/ui";
import { Button } from "@/components/ui/button";
import type { AlertSettingsView, ChannelResult } from "@/lib/obleth";

/** Where the gateway says a model is down, a pool is full, or Slurm has stalled. */
export function AlertsSection({ settings }: { settings: AlertSettingsView }) {
  const email = settings.email;
  const [emailOn, setEmailOn] = useState(!!email);
  const [testing, start] = useTransition();
  const [results, setResults] = useState<ChannelResult[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const channels = [settings.slack_webhook_set && "Slack", email && "email"].filter(Boolean) as string[];

  function test() {
    setError(null);
    setResults(null);
    start(async () => {
      const res = await testAlertAction();
      if (res.ok) setResults(res.results ?? []);
      else setError(res.error);
    });
  }

  return (
    <SettingsCard
      id="alerts"
      title="Alerts"
      description="Where the gateway says a model is down, a pool is full, or Slurm has stalled."
      action={channels.length ? <Pill><Glyph glyph="on" className="h-[7px] w-[7px]" />{channels.join(" and ")}</Pill> : <Pill inverted>No channel</Pill>}
    >
      <Setting id="set-slack" label="Slack" hint={settings.slack_webhook_set ? "Set. Type a new webhook to replace it; it's never shown." : "An incoming-webhook URL. It's never shown again."} fields={["alerts.slack_webhook_url", "alerts.clear_slack"]}>
        <TextField name="alerts.slack_webhook_url" label="Slack webhook" type="password" autoComplete="off" defaultValue="" placeholder={settings.slack_webhook_set ? "•••••••• (set)" : "https://hooks.slack.com/services/…"} mono />
        {settings.slack_webhook_set && <Switch name="alerts.clear_slack" label="Remove the Slack webhook" defaultChecked={false}>Remove the stored webhook</Switch>}
      </Setting>
      <Setting id="set-email" label="Email" hint="Sent through your SMTP server." fields={["alerts.email_enabled"]} was={{ field: "alerts.email_enabled", checkbox: true }}>
        <Switch name="alerts.email_enabled" label="Email alerts" checked={emailOn} onChange={setEmailOn} />
        {!emailOn && email && <span className="text-xs font-medium">Saving with email off deletes its server settings and password.</span>}
      </Setting>
      {emailOn && (
        <>
          <Setting label="Mail server" hint="Host and port; STARTTLS upgrades the connection." fields={["alerts.smtp_host", "alerts.smtp_port", "alerts.starttls"]}>
            <div className="flex flex-wrap items-center gap-2">
              <TextField name="alerts.smtp_host" label="SMTP host" required defaultValue={email?.smtp_host ?? ""} placeholder="smtp.example.edu" mono className="w-64" />
              <TextField name="alerts.smtp_port" label="SMTP port" inputMode="numeric" defaultValue={email?.smtp_port ?? 587} mono className="w-20" />
              <Switch name="alerts.starttls" label="STARTTLS" defaultChecked={email?.starttls ?? true}>STARTTLS</Switch>
            </div>
          </Setting>
          <Setting label="Sign-in" hint={email?.password_set ? "A password is set. Type a new one to replace it." : "Leave blank if the server needs none."} fields={["alerts.smtp_username", "alerts.smtp_password", "alerts.clear_password"]}>
            <div className="flex flex-wrap items-center gap-2">
              <TextField name="alerts.smtp_username" label="SMTP username" defaultValue={email?.username ?? ""} placeholder="Username" className="w-56" />
              <TextField name="alerts.smtp_password" label="SMTP password" type="password" autoComplete="off" defaultValue="" placeholder={email?.password_set ? "•••••••• (set)" : "Password"} className="w-56" />
            </div>
            {email?.password_set && <Switch name="alerts.clear_password" label="Remove the stored password" defaultChecked={false}>Remove the stored password</Switch>}
          </Setting>
          <Setting label="From · to" hint="One sender; recipients separated by commas or lines." fields={["alerts.from_address", "alerts.recipients"]}>
            <TextField name="alerts.from_address" label="From address" type="email" required defaultValue={email?.from_address ?? ""} placeholder="obleth@example.edu" className="max-w-sm" />
            <TextArea name="alerts.recipients" label="Recipients" rows={2} defaultValue={(email?.recipients ?? []).join(", ")} placeholder="oncall@example.edu" className="font-sans text-[13px]" />
          </Setting>
        </>
      )}
      <Setting id="set-quiet" label="Quiet period" hint="The same alert isn't sent again within this long." fields={["alerts.quiet_minutes"]} was={{ field: "alerts.quiet_minutes" }}>
        <div className="flex items-center gap-2">
          <TextField name="alerts.quiet_minutes" label="Quiet period" inputMode="decimal" defaultValue={+(settings.min_interval_secs / 60).toFixed(2)} mono className="w-20" />
          <span className="text-xs text-muted-foreground">minutes</span>
        </div>
      </Setting>
      <Setting label="Test" hint="Sends a test alert through the saved channels.">
        <div className="flex flex-wrap items-center gap-3">
          <Button type="button" variant="outline" size="sm" disabled={testing || channels.length === 0} onClick={test}>{testing ? "Sending…" : "Send a test alert"}</Button>
          <span className="text-xs text-muted-foreground">{channels.length ? "Save first: the test uses what's saved." : "Save a channel first."}</span>
        </div>
        {error && <p role="alert" className="text-xs font-medium">{error}</p>}
        {results?.map((r) => (
          <p key={r.channel} className="flex items-center gap-2 text-[12.5px]">
            {r.ok ? <Glyph glyph="on" className="h-[7px] w-[7px]" /> : <span className="rounded-full bg-foreground px-1.5 text-[10.5px] font-semibold text-background">failed</span>}
            <span className="font-medium">{r.channel}</span>
            <span className="text-muted-foreground">{r.detail}</span>
          </p>
        ))}
      </Setting>
    </SettingsCard>
  );
}
