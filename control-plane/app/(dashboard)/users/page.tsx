import { UsersList } from "@/components/access/users-list";
import { requireAdmin } from "@/lib/auth/roles";
import { listUsers } from "@/lib/auth/users";
import { obleth } from "@/lib/obleth";
import { safe } from "@/lib/safe";

export const dynamic = "force-dynamic";

export default async function UsersPage() {
  const me = await requireAdmin();
  const [users, tenants] = await Promise.all([listUsers(), safe(obleth.listTenants(), [])]);
  return <UsersList users={users} tenants={tenants.map((t) => ({ id: t.id, name: t.name })).sort((a, b) => a.name.localeCompare(b.name))} me={me.id} />;
}
