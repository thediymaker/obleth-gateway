import { requireAdmin } from "@/lib/auth/roles";
import { listUsers } from "@/lib/auth/users";
import { obleth } from "@/lib/obleth";
import { UsersManager } from "@/components/users-manager";

export const dynamic = "force-dynamic";

export default async function UsersPage() {
  await requireAdmin();
  const [users, tenants] = await Promise.all([listUsers(), obleth.listTenants()]);
  return (
    <div className="space-y-6">
      <div>
        <h1 className="text-lg font-semibold tracking-tight">Users</h1>
        <p className="text-sm text-muted-foreground">
          Approve pending accounts, assign roles, and link users to tenants.
        </p>
      </div>
      <UsersManager users={users} tenants={tenants.map((t) => ({ id: t.id, name: t.name }))} />
    </div>
  );
}
