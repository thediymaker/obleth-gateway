import { AppShell } from "@/components/app-shell";
import { getSession } from "@/lib/auth/session";
import { redirect } from "next/navigation";
import { CONTROL_PLANE_VERSION } from "@/lib/version";

export default async function DashboardLayout({ children }: { children: React.ReactNode }) {
  const session = await getSession();
  if (!session) redirect("/login");

  return (
    <AppShell username={session.username} version={CONTROL_PLANE_VERSION}>
      {children}
    </AppShell>
  );
}
