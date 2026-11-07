"use server";

import { revalidatePath } from "next/cache";
import { requireAdmin } from "@/lib/auth/roles";
import { obleth } from "@/lib/obleth";
import type { ActionResult } from "@/app/actions";

/** Stops a batch's queued requests; the ones running finish. */
export async function cancelBatchAction(id: string): Promise<ActionResult> {
  const session = await requireAdmin();
  if (!/^[A-Za-z0-9_-]{1,128}$/.test(id)) return { ok: false, error: "Not a batch id" };
  try {
    await obleth.cancelBatch(id, { auditActor: session.email });
    revalidatePath("/batches");
    return { ok: true };
  } catch (e) {
    return { ok: false, error: e instanceof Error ? e.message : String(e) };
  }
}
