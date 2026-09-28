import Link from "next/link";
import { Playground } from "@/components/playground/playground";
import { requireAdmin } from "@/lib/auth/roles";
import { obleth } from "@/lib/obleth";

export const dynamic = "force-dynamic";

export default async function PlaygroundPage({ searchParams }: { searchParams: Promise<{ model?: string }> }) {
  const user = await requireAdmin();
  const { model } = await searchParams;
  // `?model=` opens a session on that model, if it is one the playground can use.
  const target = model ? (await obleth.listModels().catch(() => [])).find((m) => m.model_name === model && m.enabled) : undefined;
  const settings = await obleth.getCharoSettings().catch(() => null);
  if (settings?.enabled === false) return <div className="p-6"><h1 className="text-xl font-semibold">Playground is disabled</h1><p className="mt-2 text-sm text-muted-foreground">Enable the chat and testing workspace in <Link className="underline" href="/settings">Settings</Link>.</p></div>;
  return (
    <Playground
      scope={user.email}
      gatewayBase={process.env.OBLETH_PROXY_BASE_URL ?? "http://localhost:8080"}
      openModel={target ? { name: target.model_name, type: target.model_type } : undefined}
    />
  );
}
