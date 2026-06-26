import { getSession } from "@/lib/auth/session";
import { redirect } from "next/navigation";

// Force-dynamic: this is a session-gated page. Avoids build-time prerender
// that would call getDb (via better-auth) without DATABASE_URL.
export const dynamic = "force-dynamic";

export default async function PortalLayout({
  children,
}: {
  children: React.ReactNode;
}) {
  const s = await getSession();
  if (!s) redirect("/login");
  if (s.status !== "active") redirect("/awaiting-approval");
  if (!s.tenantId) redirect("/awaiting-approval");
  // Admins may view the portal, but their home is the admin dashboard.
  // We do not block admins here — just gate on session/status/tenantId.
  return <div className="mx-auto max-w-4xl p-6">{children}</div>;
}
