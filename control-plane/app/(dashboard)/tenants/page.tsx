import { CreateTenant } from "@/components/create-tenant";
import { QuotaControl } from "@/components/quota-control";
import { WeightControl } from "@/components/weight-control";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { obleth, type Tenant } from "@/lib/obleth";
import { safe } from "@/lib/safe";

export const dynamic = "force-dynamic";

export default async function TenantsPage() {
  const tenants = await safe(obleth.listTenants(), [] as Tenant[]);

  return (
    <div className="space-y-6">
      <div>
        <h1 className="text-lg font-semibold tracking-tight">Tenants</h1>
        <p className="text-sm text-muted-foreground">Fairshare units — adjust weight for live priority boosts</p>
      </div>

      <Card>
        <CardHeader>
          <CardTitle>Create tenant</CardTitle>
        </CardHeader>
        <CardContent>
          <CreateTenant />
        </CardContent>
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>Tenant configuration</CardTitle>
          <CardDescription>Update request budgets and fairshare weights without leaving the table</CardDescription>
        </CardHeader>
        <CardContent className="overflow-x-auto p-0">
          <table className="w-full text-sm">
            <thead>
              <tr className="border-b border-border text-left text-xs text-muted-foreground">
                <th className="px-6 py-3 font-medium">Name</th>
                <th className="px-3 py-3 font-medium">Quotas</th>
                <th className="px-3 py-3 font-medium">Fairshare weight</th>
              </tr>
            </thead>
            <tbody>
              {tenants.map((t) => (
                <tr key={t.id} className="border-b border-border/60">
                  <td className="px-6 py-3 font-medium">{t.name}</td>
                  <td className="px-3 py-3">
                    <QuotaControl id={t.id} tokensPerMinute={t.tokens_per_minute} maxInFlight={t.max_in_flight} />
                  </td>
                  <td className="px-3 py-3">
                    <WeightControl id={t.id} initial={t.weight} />
                  </td>
                </tr>
              ))}
              {tenants.length === 0 && (
                <tr>
                  <td colSpan={3} className="px-6 py-8 text-center text-muted-foreground">
                    No tenants yet.
                  </td>
                </tr>
              )}
            </tbody>
          </table>
        </CardContent>
      </Card>
    </div>
  );
}
