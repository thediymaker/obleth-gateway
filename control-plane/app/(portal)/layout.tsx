import { redirect } from "next/navigation";
import { PortalShell } from "@/components/portal/portal-shell";
import { getSession } from "@/lib/auth/session";
import { currentPortalContext } from "@/lib/portal-context";
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

  let ctx;
  try {
    ctx = await currentPortalContext();
  } catch (e) {
    const msg = e instanceof Error ? e.message : "";
    if (msg === "No identity" || msg.startsWith("No tenant assigned")) redirect("/awaiting-approval");
    throw e;
  }

  return (
    <PortalShell
      username={session.email}
      tenants={ctx.tenants}
      current={ctx.current}
      role={session.role}
      version={CONTROL_PLANE_VERSION}
    >
      {children}
    </PortalShell>
  );
}
