/** Next.js runs register() once at server startup (Node runtime only). */
export async function register() {
  if (process.env.NEXT_RUNTIME !== "nodejs") return;
  const { applyAuthSchema } = await import("@/lib/auth/schema");
  const { bootstrapAdmin } = await import("@/lib/auth/bootstrap");
  await applyAuthSchema();
  await bootstrapAdmin();
}
