import { getDb } from "@/lib/db";
import { accounts } from "@/lib/obleth-accounts";
import { oidcProviders } from "@/lib/auth/providers";

const DISCOVERY_SUFFIX = "/.well-known/openid-configuration";

export interface PersonalDeps {
  query: (sql: string, params?: unknown[]) => Promise<{ rows: any[] }>;
  provisionIdentity: typeof accounts.provisionIdentity;
  env: Record<string, string | undefined>;
  providers: Array<{ providerId: string; discoveryUrl: string }>;
}

function defaultDeps(): PersonalDeps {
  return {
    query: (sql, params) => getDb().query(sql, params),
    provisionIdentity: (body) => accounts.provisionIdentity(body),
    env: process.env,
    providers: oidcProviders(),
  };
}

/** OIDC identity (issuer, subject) of a user, or null for credential-only users. */
export async function userIdentity(
  userId: string,
  deps: PersonalDeps = defaultDeps(),
): Promise<{ issuer: string; subject: string } | null> {
  const { rows } = await deps.query(
    'select "providerId", "accountId" from "account" where "userId" = $1 order by "createdAt" asc',
    [userId],
  );
  // A user with a password account is a credential/break-glass user: never touched.
  if (rows.some((r) => r.providerId === "credential")) return null;
  for (const row of rows) {
    const provider = deps.providers.find((p) => p.providerId === row.providerId);
    if (!provider || !provider.discoveryUrl.endsWith(DISCOVERY_SUFFIX)) continue;
    return { issuer: provider.discoveryUrl.slice(0, -DISCOVERY_SUFFIX.length), subject: row.accountId };
  }
  return null;
}

/** Provision the user's personal tenant and link it; never throws. */
export async function ensurePersonalTenant(
  userId: string,
  deps?: PersonalDeps,
): Promise<string | null> {
  try {
    const d = deps ?? defaultDeps();
    const identity = await userIdentity(userId, d);
    if (!identity) return null;
    const { rows } = await d.query('select email from "user" where id = $1', [userId]);
    const email = rows[0]?.email;
    if (!email) return null;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const provisioned = await Promise.race([
      d.provisionIdentity({ ...identity, email }),
      new Promise<never>((_, rej) => {
        timer = setTimeout(() => rej(new Error("provisionIdentity timed out")), 5000);
      }),
    ]).finally(() => clearTimeout(timer));
    await d.query(
      'update "user" set "tenantId" = $2, status = case when $3 and "tenantId" is null then \'active\' else status end where id = $1',
      [userId, provisioned.tenant_id, d.env.OIDC_AUTO_APPROVE === "true"],
    );
    return provisioned.tenant_id;
  } catch (err) {
    console.error("ensurePersonalTenant failed", err);
    return null;
  }
}
