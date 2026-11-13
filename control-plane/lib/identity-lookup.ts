import { getDb } from "@/lib/db";
import { accounts } from "@/lib/obleth-accounts";
import { ensurePersonalTenant, userIdentity } from "@/lib/auth/personal";

export const UNKNOWN_EMAIL = "User must sign in once first";
export const MULTIPLE_IDENTITIES = "Email matches multiple identities";
export const PROVISION_FAILED = "Could not provision the user's identity";

export type ResolvedIdentity = { issuer: string; subject: string; email: string };
export type IdentityResult = { ok: true; identity: ResolvedIdentity } | { ok: false; error: string };

export interface IdentityLookupDeps {
  identitiesByEmail: (email: string) => Promise<Array<{ issuer: string; subject: string; email?: string | null }>>;
  dashboardUserId: (email: string) => Promise<string | null>;
  userIdentity: (userId: string) => Promise<{ issuer: string; subject: string } | null>;
  ensurePersonalTenant: (userId: string) => Promise<string | null>;
}

const defaultDeps: IdentityLookupDeps = {
  identitiesByEmail: (email) => accounts.identitiesByEmail(email),
  dashboardUserId: async (email) => {
    const { rows } = await getDb().query('select id from "user" where lower(email) = $1 limit 2', [email.toLowerCase()]);
    return rows.length === 1 ? rows[0].id : null;
  },
  userIdentity: (userId) => userIdentity(userId),
  ensurePersonalTenant: (userId) => ensurePersonalTenant(userId),
};

/**
 * Resolve an email to a gateway identity. Falls back to a dashboard OIDC user
 * that signed in before the upgrade and has no identity yet: provisions it.
 */
export async function resolveIdentityByEmail(
  email: string,
  deps: IdentityLookupDeps = defaultDeps,
): Promise<IdentityResult> {
  const known = await deps.identitiesByEmail(email);
  if (known.length > 1) return { ok: false, error: MULTIPLE_IDENTITIES };
  if (known.length === 1) return { ok: true, identity: { issuer: known[0].issuer, subject: known[0].subject, email } };

  const userId = await deps.dashboardUserId(email);
  const ident = userId ? await deps.userIdentity(userId) : null;
  if (!userId || !ident) return { ok: false, error: UNKNOWN_EMAIL };
  if (!(await deps.ensurePersonalTenant(userId))) return { ok: false, error: PROVISION_FAILED };
  return { ok: true, identity: { ...ident, email } };
}
