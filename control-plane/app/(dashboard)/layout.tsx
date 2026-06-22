import { AppShell } from "@/components/app-shell";
import { CharoRoot } from "@/components/charo/charo-root";
import { getSession } from "@/lib/auth/session";
import { obleth } from "@/lib/obleth";
import { safe } from "@/lib/safe";
import { redirect } from "next/navigation";
import { CONTROL_PLANE_VERSION } from "@/lib/version";

export default async function DashboardLayout({ children }: { children: React.ReactNode }) {
  const session = await getSession();
  if (!session) redirect("/login");

  // Charo is enabled by default; only hide it when the operator has explicitly
  // turned it off in Settings. If the gateway is unreachable, fail open (show it).
  const charo = await safe(obleth.getCharoSettings(), { enabled: true });

  return (
    <AppShell username={session.username} version={CONTROL_PLANE_VERSION}>
      {children}
      {charo.enabled && <CharoRoot />}
    </AppShell>
  );
}
