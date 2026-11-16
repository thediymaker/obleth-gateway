import { getDb } from "@/lib/db";
import { accounts } from "@/lib/obleth-accounts";
import { ensurePersonalTenant, userIdentity } from "@/lib/auth/personal";

export interface BackfillDeps {
  query: (sql: string, params?: unknown[]) => Promise<{ rows: any[] }>;
  userIdentity: (userId: string) => Promise<{ issuer: string; subject: string } | null>;
  memberships: typeof accounts.memberships;
  upsertMember: typeof accounts.upsertMember;
  ensurePersonalTenant: (userId: string) => Promise<string | null>;
}

const defaultDeps = (): BackfillDeps => ({
  query: (sql, params) => getDb().query(sql, params),
  userIdentity: (id) => userIdentity(id),
  memberships: (issuer, subject) => accounts.memberships(issuer, subject),
  upsertMember: (tenantId, body, opts) => accounts.upsertMember(tenantId, body, opts),
  ensurePersonalTenant: (id) => ensurePersonalTenant(id),
});

/**
 * Users linked to a shared tenant through the legacy single "tenantId" column
 * become members of it (tenant_members, the many-to-many the portal reads),
 * and "tenantId" goes back to their personal tenant. Idempotent: a user
 * already a member keeps their role. Password users have no identity and keep
 * the legacy link. Returns how many users were moved.
 */
export async function backfillMemberships(deps: BackfillDeps = defaultDeps()): Promise<number> {
  const { rows } = await deps.query(
    `select u.id, u.email, u."tenantId" as "tenantId" from "user" u
       join tenants t on t.id = u."tenantId"
      where t.kind <> 'personal'`,
  );
  let moved = 0;
  for (const u of rows as Array<{ id: string; email: string; tenantId: string }>) {
    try {
      const identity = await deps.userIdentity(u.id);
      if (!identity) continue;
      const current = await deps.memberships(identity.issuer, identity.subject);
      if (!current.some((m) => m.tenant_id === u.tenantId)) {
        await deps.upsertMember(
          u.tenantId,
          { ...identity, email: u.email, role: "member" },
          { auditActor: "control-plane migration" },
        );
      }
      if (await deps.ensurePersonalTenant(u.id)) moved += 1;
    } catch (err) {
      console.error(`backfillMemberships: user ${u.id} not moved`, err);
    }
  }
  return moved;
}
