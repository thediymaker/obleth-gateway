/** Next.js runs register() once at server startup (Node runtime only). */
export async function register() {
  if (process.env.NEXT_RUNTIME !== "nodejs") return;
  const { applyAuthSchema } = await import("@/lib/auth/schema");
  const { bootstrapAdmin } = await import("@/lib/auth/bootstrap");
  await applyAuthSchema();
  await bootstrapAdmin();
  // Best effort and not awaited: the gateway may still be starting; a failure
  // is retried on the next start.
  const { backfillMemberships } = await import("@/lib/auth/backfill-memberships");
  backfillMemberships()
    .then((n) => { if (n) console.log(`backfillMemberships: moved ${n} user(s) to tenant members`); })
    .catch((err) => console.error("backfillMemberships failed", err));
}
