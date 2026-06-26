import { redirect } from "next/navigation";
import { PortalShell } from "@/components/portal/portal-shell";
import { getSession } from "@/lib/auth/session";
import { obleth, type Tenant } from "@/lib/obleth";
import { safe } from "@/lib/safe";
import { CONTROL_PLANE_VERSION } from "@/lib/version";

// Force-dynamic: this is a session-gated page. Avoids build-time prerender
// that would call getDb (via better-auth) without DATABASE_URL.
export const dynamic = "force-dynamic";

export default async function PortalLayout({
  children,
}: {
  children: React.ReactNode;
}) {
  const session = await getSession();
  if (!session) redirect("/login");
  if (session.status !== "active") redirect("/awaiting-approval");
  if (!session.tenantId) redirect("/awaiting-approval");

  const tenants = await safe(obleth.listTenants(), [] as Tenant[]);
  const tenantName =
    tenants.find((tenant) => tenant.id === session.tenantId)?.name ?? "Assigned tenant";

  return (
    <PortalShell
      username={session.email}
      tenantName={tenantName}
      role={session.role}
      version={CONTROL_PLANE_VERSION}
    >
      {children}
    </PortalShell>
  );
}
