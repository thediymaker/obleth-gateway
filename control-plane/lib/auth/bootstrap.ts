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

  const created = await auth.api.signUpEmail({
    body: { email, password, name: email },
  });
  await db.query(
    `update "user" set role = 'admin', status = 'active', "emailVerified" = true where id = $1`,
    [created.user.id],
  );
}
