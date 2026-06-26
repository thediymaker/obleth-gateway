import { requireTenant } from "@/lib/auth/roles";
import { obleth } from "@/lib/obleth";
import { formatNumber } from "@/lib/utils";

export const dynamic = "force-dynamic";

export default async function PortalUsagePage() {
  const tenantId = await requireTenant();
  const all = await obleth.usage();
  const mine = all.filter((row) => row.tenant_id === tenantId);

  const total =
    mine.length > 0
      ? mine.reduce(
          (acc, row) => ({
            requests: acc.requests + row.requests,
            input_tokens: acc.input_tokens + row.input_tokens,
            output_tokens: acc.output_tokens + row.output_tokens,
            total_tokens: acc.total_tokens + row.total_tokens,
          }),
          { requests: 0, input_tokens: 0, output_tokens: 0, total_tokens: 0 },
        )
      : null;

  return (
    <div className="space-y-4">
      <div>
        <h1 className="text-lg font-semibold">Usage</h1>
        <p className="text-sm text-muted-foreground">
          Recent usage for your tenant in the current reporting window.
        </p>
      </div>

      {mine.length === 0 ? (
        <div className="rounded-md border border-dashed border-border px-6 py-10 text-center text-sm text-muted-foreground">
          No usage data found for the current window.
        </div>
      ) : (
        <div className="space-y-4">
          {/* Summary row */}
          {total && (
            <div className="grid grid-cols-2 gap-3 sm:grid-cols-4">
              <StatCard
                label="Requests"
                value={formatNumber(total.requests)}
              />
              <StatCard
                label="Input tokens"
                value={formatNumber(total.input_tokens)}
              />
              <StatCard
                label="Output tokens"
                value={formatNumber(total.output_tokens)}
              />
              <StatCard
                label="Total tokens"
                value={formatNumber(total.total_tokens)}
              />
            </div>
          )}

          {/* Detail table */}
          <div className="overflow-x-auto rounded-md border">
            <table className="w-full text-sm">
              <thead>
                <tr className="border-b border-border bg-card/40 text-left text-xs text-muted-foreground">
                  <th className="px-4 py-2 font-medium">Requests</th>
                  <th className="px-4 py-2 text-right font-medium">
                    Input tokens
                  </th>
                  <th className="px-4 py-2 text-right font-medium">
                    Output tokens
                  </th>
                  <th className="px-4 py-2 text-right font-medium">
                    Total tokens
                  </th>
                </tr>
              </thead>
              <tbody>
                {mine.map((row, i) => (
                  <tr
                    key={i}
                    className="border-b border-border/60 last:border-0"
                  >
                    <td className="px-4 py-2 tabular-nums">
                      {formatNumber(row.requests)}
                    </td>
                    <td className="px-4 py-2 text-right tabular-nums">
                      {formatNumber(row.input_tokens)}
                    </td>
                    <td className="px-4 py-2 text-right tabular-nums">
                      {formatNumber(row.output_tokens)}
                    </td>
                    <td className="px-4 py-2 text-right tabular-nums">
                      {formatNumber(row.total_tokens)}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </div>
      )}
    </div>
  );
}

function StatCard({ label, value }: { label: string; value: string }) {
  return (
    <div className="rounded-md border border-border bg-card/50 px-4 py-3">
      <p className="text-[10px] uppercase tracking-wider text-muted-foreground">
        {label}
      </p>
      <p className="mt-0.5 text-lg font-semibold tabular-nums">{value}</p>
    </div>
  );
}
