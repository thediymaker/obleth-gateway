import { AuditLog } from "@/components/audit/audit-log";
import { buildNames, filtersFromParams, queryFor } from "@/lib/audit-model";
import { requireAdmin } from "@/lib/auth/roles";
import { obleth, type ApiKey, type AuditEntry, type KnowledgeCollection, type McpServer, type ModelRoute, type Tenant } from "@/lib/obleth";
import { safe } from "@/lib/safe";

export const dynamic = "force-dynamic";

export default async function AuditPage({ searchParams }: { searchParams: Promise<Record<string, string | undefined>> }) {
  await requireAdmin();
  const filters = filtersFromParams(await searchParams);
  const [entries, models, tenants, keys, mcp, collections] = await Promise.all([
    safe(obleth.auditQuery({ ...queryFor(filters), limit: 500 }), [] as AuditEntry[]),
    safe(obleth.listModels(), [] as ModelRoute[]),
    safe(obleth.listTenants(), [] as Tenant[]),
    safe(obleth.listKeys(), [] as ApiKey[]),
    safe(obleth.listMcpServers(), [] as McpServer[]),
    safe(obleth.listCollections(), [] as KnowledgeCollection[]),
  ]);
  // Names of deleted things come from the log itself, so they're looked up over a wider window.
  const history = await safe(obleth.auditQuery({ limit: 1000, action: "create_model,create_tenant,create_key,create_mcp_server,create_knowledge_collection,upload_knowledge_document,create_recipe" }), [] as AuditEntry[]);
  const current: Record<string, string> = {};
  for (const m of models) current[m.id] = m.model_name;
  for (const t of tenants) current[t.id] = t.name;
  for (const k of keys) current[k.id] = k.name || k.key_prefix;
  for (const s of mcp) current[s.id] = s.name;
  for (const c of collections) current[c.id] = c.name;
  const names = buildNames(current, [...entries, ...history]);
  return <AuditLog key={JSON.stringify(filters)} entries={entries} filters={filters} names={names} alive={Object.keys(current)} total={null} />;
}
