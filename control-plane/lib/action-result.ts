/** What a server action answers: done (with any warnings), or why not. */
export type ActionResult =
  | { ok: true; warnings?: string[] }
  | { ok: false; error: string };

/**
 * A failed action's answer. A gateway refusal (`OblethApiError`) carries the
 * gateway's own message, so the caller sees why.
 */
export function actionError(e: unknown): { ok: false; error: string } {
  return { ok: false, error: e instanceof Error ? e.message : "Unexpected error" };
}
