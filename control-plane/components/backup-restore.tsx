"use client";

import { useRef, useState, useTransition } from "react";
import type { ChangeEvent } from "react";
import { Download, Upload } from "lucide-react";
import { restoreBackupAction } from "@/app/actions";
import type { ConfigBackup, RestoreCounts, RestoreReport } from "@/lib/obleth";
import { Button } from "@/components/ui/button";
import {
  Card,
  CardContent,
  CardDescription,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { DestructiveConfirm } from "@/components/ui/destructive-confirm";

const ENTITIES = [
  ["fairshare_groups", "Fairshare groups"],
  ["tenants", "Tenants"],
  ["api_keys", "API keys"],
  ["models", "Models"],
  ["model_endpoints", "Model endpoints"],
  ["mcp_servers", "MCP servers"],
  ["app_settings", "Settings"],
] as const;

type EntityKey = (typeof ENTITIES)[number][0];

export function BackupRestore() {
  const [pending, start] = useTransition();
  const fileInputRef = useRef<HTMLInputElement>(null);

  // Restore flow: file picked -> parsed preview -> confirm dialog -> report.
  const [restoreText, setRestoreText] = useState("");
  const [preview, setPreview] = useState<ConfigBackup | null>(null);
  const [confirmOpen, setConfirmOpen] = useState(false);
  const [report, setReport] = useState<RestoreReport | null>(null);
  const [error, setError] = useState<string | null>(null);

  function onRestoreFile(event: ChangeEvent<HTMLInputElement>) {
    const file = event.target.files?.[0];
    event.target.value = "";
    if (!file) return;
    setReport(null);
    setError(null);
    setPreview(null);
    const reader = new FileReader();
    reader.onload = () => {
      const text = String(reader.result ?? "");
      let parsed: ConfigBackup;
      try {
        parsed = JSON.parse(text) as ConfigBackup;
      } catch {
        setError("Not a valid JSON file.");
        return;
      }
      if (!parsed || parsed.format !== "obleth-config-backup") {
        setError("Not an obleth config backup file.");
        return;
      }
      setRestoreText(text);
      setPreview(parsed);
      setConfirmOpen(true);
    };
    reader.onerror = () => setError("Could not read the selected file.");
    reader.readAsText(file);
  }

  function confirmRestore() {
    if (!restoreText) return;
    start(async () => {
      const result = await restoreBackupAction(restoreText);
      setConfirmOpen(false);
      setPreview(null);
      setRestoreText("");
      if (result.ok) {
        setReport(result.report);
      } else {
        setError(result.error);
      }
    });
  }

  function cancelRestore(open: boolean) {
    setConfirmOpen(open);
    if (!open && !pending) {
      setPreview(null);
      setRestoreText("");
    }
  }

  const previewCounts = preview
    ? ENTITIES.map(([key, label]) => ({
        label,
        count: (preview.data?.[key as EntityKey] ?? []).length,
      })).filter((e) => e.count > 0)
    : [];

  return (
    <Card>
      <CardHeader>
        <CardTitle>Config backup</CardTitle>
        <CardDescription>
          Export a JSON snapshot of all gateway configuration&mdash;fairshare groups, tenants, API
          keys, models, endpoints, MCP servers, and settings&mdash;or restore one. Usage history is
          not included. Provider secrets are carried as encrypted ciphertext, so restoring requires
          the same <code className="font-mono text-xs">OBLETH_ENCRYPTION_KEY</code>; alert
          credentials (Slack webhook, SMTP password) are included as stored.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        <div className="flex flex-wrap items-center gap-2">
          <Button
            type="button"
            variant="outline"
            onClick={() => {
              setReport(null);
              setError(null);
              window.location.href = "/api/live/backup/export";
            }}
          >
            <Download className="mr-2 h-4 w-4" />
            Download backup
          </Button>
          <Button
            type="button"
            variant="outline"
            disabled={pending}
            onClick={() => fileInputRef.current?.click()}
          >
            <Upload className="mr-2 h-4 w-4" />
            Restore from backup&hellip;
          </Button>
          <input
            ref={fileInputRef}
            type="file"
            accept=".json,application/json"
            className="hidden"
            onChange={onRestoreFile}
          />
        </div>
        <p className="text-xs text-muted-foreground">
          Restore merges: entities in the backup are created or updated by id; anything that exists
          only on this instance is left untouched. Existing client API keys keep working after a
          restore.
        </p>

        {error ? (
          <p className="text-sm text-destructive" role="alert">
            {error}
          </p>
        ) : null}

        {report ? (
          <div className="space-y-2 rounded-md border border-border bg-muted/30 p-3 text-sm">
            <p className="font-medium">Restore complete.</p>
            <ul className="grid grid-cols-1 gap-x-6 gap-y-0.5 sm:grid-cols-2">
              {ENTITIES.map(([key, label]) => {
                const counts: RestoreCounts | undefined = report[key as EntityKey];
                if (!counts || counts.inserted + counts.updated === 0) return null;
                return (
                  <li key={key} className="text-muted-foreground">
                    {label}: {counts.inserted} added, {counts.updated} updated
                  </li>
                );
              })}
            </ul>
            {report.warnings?.length ? (
              <ul className="space-y-1 text-amber-600 dark:text-amber-500">
                {report.warnings.map((w) => (
                  <li key={w}>{w}</li>
                ))}
              </ul>
            ) : null}
          </div>
        ) : null}

        <DestructiveConfirm
          open={confirmOpen}
          onOpenChange={cancelRestore}
          title="Restore config backup"
          confirmWord="RESTORE"
          checkboxLabel="I understand this overwrites the configuration of every entity present in the backup."
          confirmLabel="Restore backup"
          pending={pending}
          onConfirm={confirmRestore}
          description={
            preview ? (
              <>
                <p>
                  Backup from {new Date(preview.exported_at).toLocaleString()} (gateway{" "}
                  {preview.gateway_version}) containing{" "}
                  {previewCounts.length > 0
                    ? previewCounts.map((e) => `${e.count} ${e.label.toLowerCase()}`).join(", ")
                    : "no entities"}
                  .
                </p>
                <p>
                  The restore is a merge: backup entities are created or updated by id, nothing is
                  deleted, and it applies atomically&mdash;all or nothing.
                </p>
              </>
            ) : null
          }
        />
      </CardContent>
    </Card>
  );
}
