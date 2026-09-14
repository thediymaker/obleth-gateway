"use client";

import type { ModelImportEntry, ModelImportReport } from "@/lib/obleth";
import { Button } from "@/components/ui/button";

/// Shared review UI for the model manifest import, used by both entry points:
/// the Models toolbar (file upload) and the provider-discovery wizard.
///
/// Both run the same two-step flow — apply as a dry run, show this preview,
/// then commit — so the operator always sees the per-model diff before
/// anything is written.

const ACTION_STYLES: Record<string, string> = {
  created: "text-emerald-600 dark:text-emerald-500",
  updated: "text-amber-600 dark:text-amber-500",
  unchanged: "text-muted-foreground",
};

export function ManifestEntryList({
  entries,
  showUnchanged,
}: {
  entries: ModelImportEntry[];
  showUnchanged: boolean;
}) {
  const shown = showUnchanged
    ? entries
    : entries.filter((e) => e.action !== "unchanged");
  if (shown.length === 0) return null;

  return (
    <ul className="max-h-64 space-y-2 overflow-y-auto text-sm">
      {shown.map((entry) => (
        <li key={entry.model_name} className="space-y-0.5">
          <div className="flex flex-wrap items-baseline gap-2">
            <span className="font-mono text-xs">{entry.model_name}</span>
            <span
              className={ACTION_STYLES[entry.action] ?? "text-muted-foreground"}
            >
              {entry.action}
            </span>
          </div>
          {entry.changed_fields.length > 0 ? (
            <p className="text-xs text-muted-foreground">
              {entry.changed_fields.join(", ")}
            </p>
          ) : null}
          {entry.warnings.map((w) => (
            <p key={w} className="text-xs text-amber-600 dark:text-amber-500">
              {w}
            </p>
          ))}
        </li>
      ))}
    </ul>
  );
}

/// Dry-run preview: what the file would do, with nothing written yet.
export function ManifestPreview({
  report,
  pending,
  onConfirm,
  onCancel,
}: {
  report: ModelImportReport;
  pending: boolean;
  onConfirm: () => void;
  onCancel: () => void;
}) {
  const willWrite = report.created + report.updated;

  return (
    <div className="space-y-3 rounded-md border border-border bg-muted/30 p-3">
      <p className="text-sm">
        {willWrite === 0
          ? `All ${report.unchanged} model(s) in this file already match the gateway — applying it would change nothing.`
          : `${report.created} model(s) would be added, ${report.updated} updated, ${report.unchanged} left unchanged. Nothing has been written yet.`}
      </p>
      <ManifestEntryList entries={report.models} showUnchanged />
      <div className="flex flex-wrap items-center gap-2">
        <Button
          type="button"
          size="sm"
          disabled={pending || willWrite === 0}
          onClick={onConfirm}
        >
          {pending ? "Applying…" : `Apply to ${willWrite} model(s)`}
        </Button>
        <Button
          type="button"
          size="sm"
          variant="outline"
          disabled={pending}
          onClick={onCancel}
        >
          Cancel
        </Button>
      </div>
    </div>
  );
}

/// Post-apply summary.
export function ManifestResultBanner({
  report,
  onDismiss,
}: {
  report: ModelImportReport;
  onDismiss: () => void;
}) {
  return (
    <div className="space-y-2 rounded-md border border-border bg-muted/30 p-3">
      <div className="flex items-start justify-between gap-3">
        <p className="text-sm font-medium">
          Import complete: {report.created} added, {report.updated} updated,{" "}
          {report.unchanged} unchanged.
        </p>
        <button
          type="button"
          onClick={onDismiss}
          className="shrink-0 text-xs underline opacity-80 hover:opacity-100"
        >
          Dismiss
        </button>
      </div>
      <ManifestEntryList entries={report.models} showUnchanged={false} />
    </div>
  );
}
