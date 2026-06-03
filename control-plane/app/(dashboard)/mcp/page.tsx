import { McpManager } from "@/components/mcp-manager";
import { obleth } from "@/lib/obleth";
import { safe } from "@/lib/safe";

export const dynamic = "force-dynamic";

export default async function McpPage() {
  const servers = await safe(obleth.listMcpServers(), []);

  return (
    <div className="space-y-6">
      <div>
        <h1 className="text-lg font-semibold tracking-tight">MCP Servers</h1>
        <p className="text-sm text-muted-foreground">
          Reverse-proxy Model Context Protocol servers through obleth&apos;s auth and audit layer.
        </p>
      </div>
      <McpManager servers={servers} />
    </div>
  );
}
