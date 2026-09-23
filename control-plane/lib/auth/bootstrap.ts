import { auth } from "@/lib/auth/better-auth";

/** better-auth's own sign-up minimum; the internal adapter does not enforce it. */
const MIN_PASSWORD_LENGTH = 8;

/**
 * Seed a single local email+password admin if no active admin exists yet. This
 * is the break-glass account: it works even when the IdP is unreachable. No-op
 * once an active admin exists, so it is safe to run on every boot.
 *
 * Seeds through better-auth's internal adapter (the same createUser + credential
 * linkAccount pair its sign-up route runs) rather than an HTTP-style endpoint:
 * public sign-up is disabled, and `/admin/create-user` is refused without an
 * active admin session by `requireActiveAccountForAdminApi` — which, on a fresh
 * database, nobody can have.
 */
export async function bootstrapAdmin(): Promise<void> {
  const email = process.env.DASHBOARD_ADMIN_EMAIL?.trim().toLowerCase();
  const password = process.env.DASHBOARD_PASSWORD;
  if (!email || !password) return; // nothing to seed (e.g. SSO-only deploy)

  // Database errors propagate: startup must fail loudly when it cannot tell
  // whether an admin exists, rather than boot "healthy" without one.
  const ctx = await auth.$context;
  const activeAdmins = await ctx.adapter.count({
    model: "user",
    where: [
      { field: "role", value: "admin" },
      { field: "status", value: "active" },
    ],
  });
  if (activeAdmins > 0) return;

  // A misconfigured break-glass account must not crash startup: the server can
  // still serve SSO, and the operator can fix the config and restart.
  if (password.length < MIN_PASSWORD_LENGTH) {
    console.error(
      "[auth] failed to seed break-glass admin: password too short. " +
        `Set DASHBOARD_PASSWORD to >=${MIN_PASSWORD_LENGTH} chars (better-auth minimum).`,
    );
    return;
  }
  if (await ctx.internalAdapter.findUserByEmail(email)) {
    // Never silently re-promote or reactivate an existing account.
    console.error(
      `[auth] failed to seed break-glass admin: a user with email ${email} already exists but is not an active admin.`,
    );
    return;
  }
  const hash = await ctx.password.hash(password);
  const user = await ctx.internalAdapter.createUser({
    email,
    name: email,
    emailVerified: true,
    role: "admin",
    status: "active",
  });
  await ctx.internalAdapter.linkAccount({
    userId: user.id,
    providerId: "credential",
    accountId: user.id,
    password: hash,
  });
}
