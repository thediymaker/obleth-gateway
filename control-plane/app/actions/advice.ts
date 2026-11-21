"use server";

/** Advice (Settings > Needs you). */

import { revalidatePath } from "next/cache";
import { obleth } from "@/lib/obleth";
import { requireAdmin } from "@/lib/auth/roles";
import { actionError, type ActionResult } from "@/lib/action-result";

/**
 * Make the change a finding offers. The finding is read again from the
 * gateway, so the action applies what the gateway says now, not what the page
 * showed. Each model is written with only its boons changed.
 */
export async function applyAdviceFixAction(key: string): Promise<ActionResult> {
  const session = await requireAdmin();
  const audit = { auditActor: session.email };
  try {
    const report = await obleth.getAdvice();
    const fix = report.findings.find((f) => f.key === key)?.fix;
    if (!fix) return { ok: false, error: "That finding has no fix to apply any more. Reload the page." };
    if (fix.kind === "grant_boon") {
      const models = await obleth.listModels();
      const failed: string[] = [];
      for (const name of fix.models) {
        const m = models.find((x) => x.model_name === name);
        if (!m) { failed.push(`${name}: no longer registered`); continue; }
        if ((m.boons ?? []).includes(fix.boon)) continue;
        try {
          await obleth.updateModel(m.id, { upstream_model: m.upstream_model, api_base: m.api_base, boons: [...(m.boons ?? []), fix.boon] }, audit);
        } catch (e) {
          failed.push(`${name}: ${e instanceof Error ? e.message : String(e)}`);
        }
      }
      revalidatePath("/settings");
      revalidatePath("/models");
      if (failed.length === fix.models.length) return { ok: false, error: failed.join("; ") };
      return failed.length ? { ok: true, warnings: failed } : { ok: true };
    }
    return { ok: false, error: "This kind of fix can't be applied from here." };
  } catch (e) {
    return actionError(e);
  }
}

/** Set a finding aside until `untilMs`, or for good when it is left out. */
export async function snoozeAdviceAction(key: string, title: string, untilMs?: number): Promise<ActionResult> {
  const session = await requireAdmin();
  try {
    await obleth.snoozeAdvice(key, { title, ...(untilMs ? { until_ms: untilMs } : {}) }, { auditActor: session.email });
  } catch (e) {
    return actionError(e);
  }
  revalidatePath("/settings");
  return { ok: true };
}

/** Bring a set-aside finding back. */
export async function restoreAdviceAction(key: string): Promise<ActionResult> {
  const session = await requireAdmin();
  try {
    await obleth.restoreAdvice(key, { auditActor: session.email });
  } catch (e) {
    return actionError(e);
  }
  revalidatePath("/settings");
  return { ok: true };
}
