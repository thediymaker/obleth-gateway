"use client";

import { useState, useTransition } from "react";
import { Save, Send, Zap } from "lucide-react";
import { setEnergySettingsAction, testEnergyQueryAction } from "@/app/actions";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import type { EnergySettingsView } from "@/lib/obleth";

// Preserves an explicit 0 (renewable campus → carbon 0; free power → cost 0).
// Only falls back when the field is blank or unparseable.
const parseRate = (x: string, fallback: number): number =>
  x.trim() === "" || Number.isNaN(Number(x)) ? fallback : Number(x);

export function EnergySettingsForm({
  settings,
}: {
  settings: EnergySettingsView | null;
}) {
  const [pending, start] = useTransition();
  const [testing, startTest] = useTransition();
  const [status, setStatus] = useState<{ ok: boolean; message: string } | null>(
    null,
  );
  const [testResult, setTestResult] = useState<string | null>(null);

  const [enabled, setEnabled] = useState(settings?.enabled ?? false);
  const [prometheusUrl, setPrometheusUrl] = useState(
    settings?.prometheus_url ?? "",
  );
  const [powerQuery, setPowerQuery] = useState(settings?.power_query ?? "");
  const [pollInterval, setPollInterval] = useState(
    String(settings?.poll_interval_secs ?? 60),
  );
  const [costPerKwh, setCostPerKwh] = useState(
    String(settings?.energy_cost_per_kwh ?? 0.1),
  );
  const [carbonPerKwh, setCarbonPerKwh] = useState(
    String(settings?.carbon_g_per_kwh ?? 400),
  );
  const [pue, setPue] = useState(String(settings?.pue ?? 1.0));

  function save() {
    setStatus(null);
    start(async () => {
      const result = await setEnergySettingsAction({
        enabled,
        prometheus_url: prometheusUrl.trim(),
        power_query: powerQuery.trim(),
        poll_interval_secs: Number(pollInterval) || 60,
        energy_cost_per_kwh: parseRate(costPerKwh, 0.1),
        carbon_g_per_kwh: parseRate(carbonPerKwh, 400),
        pue: Number(pue) || 1.0,
      });
      setStatus(
        result.ok
          ? { ok: true, message: "Energy settings saved." }
          : { ok: false, message: result.error },
      );
    });
  }

  function runTest() {
    setStatus(null);
    setTestResult(null);
    startTest(async () => {
      const result = await testEnergyQueryAction(
        prometheusUrl.trim(),
        powerQuery.trim(),
      );
      if (result.ok && result.data) {
        const kw = (result.data.cluster_watts / 1000).toFixed(1);
        setTestResult(`${kw} kW across ${result.data.node_count} nodes`);
      } else if (!result.ok) {
        setStatus({ ok: false, message: result.error });
      }
    });
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          <Zap className="h-4 w-4" />
          Energy accounting
        </CardTitle>
        <CardDescription>
          Track cluster power draw and carbon emissions via a Prometheus power
          query. Polling runs on every interval and is surfaced in usage reports
          and per-tenant chargeback.
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
          Enable energy accounting
        </label>

        <div className="space-y-1.5">
          <Label htmlFor="prometheus_url">Prometheus URL</Label>
          <Input
            id="prometheus_url"
            type="url"
            value={prometheusUrl}
            onChange={(e) => setPrometheusUrl(e.target.value)}
            placeholder="http://prometheus:9090"
          />
        </div>

        <div className="space-y-1.5">
          <Label htmlFor="power_query">Power query (PromQL)</Label>
          <textarea
            id="power_query"
            value={powerQuery}
            onChange={(e) => setPowerQuery(e.target.value)}
            rows={3}
            className="flex w-full rounded-md border border-input bg-background px-3 py-2 text-sm"
            placeholder="habana_gaudi_module_power_watts"
          />
          <p className="text-xs text-muted-foreground">
            PromQL returning one power series (watts) per node — obleth uses
            sum() for cluster watts and count() for node count.
          </p>
        </div>

        <div className="grid gap-4 sm:grid-cols-2">
          <div className="space-y-1.5">
            <Label htmlFor="poll_interval_secs">Poll interval (seconds)</Label>
            <Input
              id="poll_interval_secs"
              type="number"
              min={10}
              value={pollInterval}
              onChange={(e) => setPollInterval(e.target.value)}
            />
          </div>
          <div className="space-y-1.5">
            <Label htmlFor="energy_cost_per_kwh">Electricity rate ($/kWh)</Label>
            <Input
              id="energy_cost_per_kwh"
              type="number"
              step="0.001"
              min={0}
              value={costPerKwh}
              onChange={(e) => setCostPerKwh(e.target.value)}
            />
          </div>
          <div className="space-y-1.5">
            <Label htmlFor="carbon_g_per_kwh">
              Carbon intensity (gCO&#8322;/kWh)
            </Label>
            <Input
              id="carbon_g_per_kwh"
              type="number"
              min={0}
              value={carbonPerKwh}
              onChange={(e) => setCarbonPerKwh(e.target.value)}
            />
          </div>
          <div className="space-y-1.5">
            <Label htmlFor="pue">PUE</Label>
            <Input
              id="pue"
              type="number"
              step="0.01"
              min={1}
              value={pue}
              onChange={(e) => setPue(e.target.value)}
            />
            <p className="text-xs text-muted-foreground">
              1.0 = IT power only; set your facility PUE to include cooling
              overhead.
            </p>
          </div>
        </div>

        <div className="flex flex-wrap items-center gap-3 border-t border-border/60 pt-4">
          <Button onClick={save} disabled={pending}>
            <Save className="h-4 w-4" />
            {pending ? "Saving..." : "Save energy"}
          </Button>
          <Button variant="outline" onClick={runTest} disabled={testing}>
            <Send className="mr-2 h-4 w-4" />
            {testing ? "Testing..." : "Test query"}
          </Button>
          {testResult && (
            <span className="rounded-md border border-emerald-500/40 bg-emerald-500/10 px-3 py-2 text-sm text-emerald-600 dark:text-emerald-400">
              {testResult}
            </span>
          )}
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
    </Card>
  );
}
