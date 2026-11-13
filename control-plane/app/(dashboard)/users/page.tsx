import { UsersList, type UserTenants } from "@/components/access/users-list";
import { userIdentity } from "@/lib/auth/personal";
import { requireAdmin } from "@/lib/auth/roles";
import { listUsers } from "@/lib/auth/users";
import { getDb } from "@/lib/db";
import { obleth } from "@/lib/obleth";
import { accounts, type Membership } from "@/lib/obleth-accounts";
import { safe } from "@/lib/safe";

export const dynamic = "force-dynamic";

export default async function UsersPage() {
  const me = await requireAdmin();
  const [users, tenants, personal] = await Promise.all([
    listUsers(),
    safe(obleth.listTenants(), []),
    safe(getDb().query("select id from tenants where kind = 'personal'").then((r) => new Set(r.rows.map((x: { id: string }) => x.id))), new Set<string>()),
  ]);
  const access: Record<string, UserTenants> = Object.fromEntries(
    await Promise.all(
      users.map(async (u) => {
        const identity = await safe(userIdentity(u.id), null);
        // null = couldn't load: the UI then shows and changes nothing, rather than an empty set it would sync.
        const memberships = identity ? await safe(accounts.memberships(identity.issuer, identity.subject), null as Membership[] | null) : [];
        const shared = memberships?.filter((m) => m.kind !== "personal").map((m) => ({ id: m.tenant_id, name: m.tenant_name, role: m.role })) ?? null;
        return [u.id, { member: identity != null, tenants: shared }] as const;
      }),
    ),
  );
  const shared = tenants.filter((t) => !personal.has(t.id)).map((t) => ({ id: t.id, name: t.name })).sort((a, b) => a.name.localeCompare(b.name));
  return <UsersList users={users} tenants={shared} access={access} me={me.id} />;
}
