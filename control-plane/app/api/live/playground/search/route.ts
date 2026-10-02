import { NextRequest, NextResponse } from "next/server";
import { z } from "zod";
import { guardAdmin } from "@/lib/auth/guard";
import { gatewaySearch, type GatewaySearchBody } from "@/lib/charo/gateway";
import { apiErrorMessage } from "@/lib/charo/errors";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

// The playground's web search relay. Calls the gateway's normal /v1/search
// data-plane endpoint as the reserved internal tenant, so admission and the
// request log behave exactly as for a real client. The limits mirror the
// gateway's (search.rs), which stays the authority.
const bodySchema = z.object({
  tool: z.string().min(1).max(200),
  query: z.string().trim().min(1).max(2000),
  maxResults: z.number().int().min(1).max(20).optional(),
  domains: z.array(z.string().trim().min(1).max(253)).max(20).optional(),
  timeRange: z.enum(["day", "week", "month", "year"]).optional(),
});

export async function POST(req: NextRequest) {
  const denied = await guardAdmin();
  if (denied) return denied;

  const parsed = bodySchema.safeParse(await req.json().catch(() => null));
  if (!parsed.success) {
    return NextResponse.json({ error: "Invalid search request." }, { status: 400 });
  }
  const { tool, query, maxResults, domains, timeRange } = parsed.data;
  const body: GatewaySearchBody = {
    search_tool_name: tool,
    query,
    ...(maxResults !== undefined ? { max_results: maxResults } : {}),
    ...(domains?.length ? { search_domain_filter: domains } : {}),
    ...(timeRange ? { time_range: timeRange } : {}),
  };
  const started = Date.now();
  try {
    const upstream = await gatewaySearch(body);
    const latencyMs = Date.now() - started;
    const requestId = upstream.headers.get("x-obleth-request-id");
    const json = await upstream.json().catch(() => null);
    if (!upstream.ok) {
      return NextResponse.json(
        { error: apiErrorMessage(json, "search failed"), requestId },
        { status: upstream.status },
      );
    }
    return NextResponse.json({
      results: Array.isArray(json?.results) ? json.results : [],
      latencyMs,
      requestId,
    });
  } catch (e) {
    return NextResponse.json({ error: String(e) }, { status: 502 });
  }
}
