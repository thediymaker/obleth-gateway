"use server";

/** Config backup restore and model manifest import. */

import { revalidatePath } from "next/cache";
import { parse as parseYaml } from "yaml";
import { obleth } from "@/lib/obleth";
import type { ConfigBackup, ModelImportReport, ModelManifest, RestoreReport } from "@/lib/obleth";
import { requireAdmin } from "@/lib/auth/roles";
import { actionError } from "@/lib/action-result";
import { trimmed } from "@/lib/form-helpers";

export type RestoreBackupResult =
  | { ok: true; report: RestoreReport }
  | { ok: false; error: string };

// Restores an uploaded obleth config backup. The file is parsed once to check
// its format here, then its text (not a re-serialised copy) goes to the admin
// API's atomic merge-restore;
// the gateway rejects it up front when its encryption key doesn't match the
// backup's. Every entity list the restore can touch is revalidated.
export async function restoreBackupAction(
  text: string,
): Promise<RestoreBackupResult> {
  const session = await requireAdmin();

  let format: unknown;
  try {
    format = (JSON.parse(text) as Partial<ConfigBackup> | null)?.format;
  } catch {
    return { ok: false, error: "Not a valid JSON file." };
  }
  if (format !== "obleth-config-backup") {
    return { ok: false, error: "Not an obleth config backup file." };
  }

  try {
    const report = await obleth.restoreBackup(text, { auditActor: session.email });
    revalidatePath("/");
    revalidatePath("/tenants");
    revalidatePath("/keys");
    revalidatePath("/models");
    revalidatePath("/mcp");
    revalidatePath("/fairshare");
    revalidatePath("/settings");
    return { ok: true, report };
  } catch (e) {
    return actionError(e);
  }
}

export type ApplyManifestResult =
  | { ok: true; report: ModelImportReport }
  | { ok: false; error: string };

// Parses an uploaded model file into a manifest the gateway will accept.
//
// Two shapes are allowed. A current manifest carries `format: "obleth-models"`.
// A bare `models:` list — the shape the older models template used — is
// accepted too and wrapped, so template files written before the manifest
// existed keep working. YAML and JSON both parse; YAML is a JSON superset, so
// one parser covers the JSON case when a strict JSON parse fails.
function readModelManifest(
  text: string,
): { manifest: ModelManifest } | { error: string } {
  if (!text.trim()) return { error: "No file content provided." };

  let parsed: unknown;
  const trimmed = text.trim();
  try {
    parsed =
      trimmed.startsWith("{") || trimmed.startsWith("[")
        ? JSON.parse(trimmed)
        : parseYaml(trimmed);
  } catch {
    try {
      parsed = parseYaml(trimmed);
    } catch {
      return { error: "Could not parse the file as JSON or YAML." };
    }
  }

  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
    return { error: "Expected a document with a top-level `models` list." };
  }
  const doc = parsed as Record<string, unknown>;

  const isManifest = doc.format === "obleth-models";
  if (doc.format != null && !isManifest) {
    return {
      error: `Not an obleth model file (format ${JSON.stringify(doc.format)}).`,
    };
  }
  if (!Array.isArray(doc.models)) {
    return { error: "Expected a document with a top-level `models` list." };
  }
  if (doc.models.length === 0) {
    return { error: "The file lists no models." };
  }
  const bad = doc.models.findIndex(
    (m) =>
      !m ||
      typeof m !== "object" ||
      typeof (m as Record<string, unknown>).model_name !== "string" ||
      !(m as Record<string, unknown>).model_name,
  );
  if (bad >= 0) {
    return { error: `Model at position ${bad + 1} has no model_name.` };
  }

  return {
    manifest: {
      format: "obleth-models",
      // Only a real manifest's version is meaningful; a bare `models:` list
      // may carry an unrelated `version` from the older template shape.
      version: isManifest && typeof doc.version === "number" ? doc.version : 1,
      models: doc.models as ModelManifest["models"],
    },
  };
}

// Applies an uploaded model manifest. Call with dryRun to validate and preview
// the per-model diff without writing; call again with dryRun false to commit.
// The gateway validates the whole file before touching anything, so a rejected
// manifest leaves the registry exactly as it was.
export async function applyModelManifestAction(
  text: string,
  dryRun: boolean,
): Promise<ApplyManifestResult> {
  const session = await requireAdmin();

  const read = readModelManifest(text);
  if ("error" in read) return { ok: false, error: read.error };

  try {
    const report = await obleth.importModels(read.manifest, {
      dryRun,
      auditActor: session.email,
    });
    // A dry run writes nothing, so there is nothing to revalidate.
    if (!dryRun) {
      revalidatePath("/models");
      revalidatePath("/");
    }
    return { ok: true, report };
  } catch (e) {
    return actionError(e);
  }
}
