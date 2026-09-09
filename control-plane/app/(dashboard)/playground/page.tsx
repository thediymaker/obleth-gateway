import Link from "next/link";
import { Playground } from "@/components/playground/playground";
import { requireAdmin } from "@/lib/auth/roles";
import { obleth } from "@/lib/obleth";

export const dynamic = "force-dynamic";

export default async function PlaygroundPage() {
  const user = await requireAdmin();
  const settings = await obleth.getCharoSettings().catch(() => null);
  if (settings?.enabled === false) return <div className="p-6"><h1 className="text-xl font-semibold">Playground is disabled</h1><p className="mt-2 text-sm text-muted-foreground">Enable the chat and testing workspace in <Link className="underline" href="/settings">Settings</Link>.</p></div>;
  return <Playground scope={user.email} />;
}
