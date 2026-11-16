import { redirect } from "next/navigation";
import { PortalMembers } from "@/components/portal/portal-members";
import { currentPortalContext } from "@/lib/portal-context";
import { accounts, type Member } from "@/lib/obleth-accounts";
import { listUsers } from "@/lib/auth/users";
import { safe } from "@/lib/safe";

export const dynamic = "force-dynamic";

export default async function PortalMembersPage({ searchParams }: { searchParams: Promise<{ error?: string; ok?: string }> }) {
  const { error, ok } = await searchParams;
  const ctx = await currentPortalContext();
  if (ctx.current.kind === "personal") {
    return <p className="text-sm text-muted-foreground">Personal tenants have no members</p>;
  }
  if (ctx.current.role !== "manager") redirect("/portal");
  const [members, users] = await Promise.all([
    safe(accounts.listMembers(ctx.current.id), [] as Member[]),
    safe(listUsers(), []),
  ]);
  const emails = [...new Set(users.map((u) => u.email.toLowerCase()))].sort();
  return <PortalMembers members={members} me={ctx.identity} users={emails} error={error} ok={ok} />;
}
