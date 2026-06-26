import { auth } from "@/lib/auth/better-auth";
import { getDb } from "@/lib/db";

/**
 * Seed a single local email+password admin if no admin exists yet. This is the
 * break-glass account: it works even when the IdP is unreachable. No-op once an
 * admin exists, so it is safe to run on every boot.
 */
export async function bootstrapAdmin(): Promise<void> {
  const email = process.env.DASHBOARD_ADMIN_EMAIL ?? process.env.DASHBOARD_USERNAME;
  const password = process.env.DASHBOARD_PASSWORD;
  if (!email || !password) return; // nothing to seed (e.g. SSO-only deploy)

  const db = getDb();
  const { rows } = await db.query<{ count: string }>(
    `select count(*)::int as count from "user" where role = 'admin'`,
  );
  if (Number(rows[0]?.count ?? 0) > 0) return;

  try {
    const created = await auth.api.signUpEmail({
      body: { email, password, name: email },
    });
    await db.query(
      `update "user" set role = 'admin', status = 'active', "emailVerified" = true where id = $1`,
      [created.user.id],
    );
  } catch (err) {
    // Don't let a misconfigured break-glass admin (e.g. a password that fails
    // better-auth's minimum length) crash startup: the server can still boot and
    // serve SSO, and the operator can fix the config and restart.
    const message = err instanceof Error ? err.message : String(err);
    console.error(
      `[auth] failed to seed break-glass admin: ${message}. ` +
        `Set DASHBOARD_PASSWORD to >=8 chars (better-auth minimum).`,
    );
  }
}
