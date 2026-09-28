import { NextRequest, NextResponse } from "next/server";
import { z } from "zod";
import { guardAdmin } from "@/lib/auth/guard";
import { gatewayVerdicts, type GatewayVerdictsBody } from "@/lib/charo/gateway";
import { apiErrorMessage } from "@/lib/charo/errors";

export const runtime = "nodejs";
export const dynamic = "force-dynamic";

// The playground's typed-verdict relay. Calls the gateway's normal
// /v1/verdicts data-plane endpoint as the reserved internal tenant, so
// admission, billing, and telemetry behave exactly as for a real client.
//
// The limits mirror the gateway's validator (verdicts/types.rs) — the gateway
// remains the authority; these exist so obviously bad requests fail fast with
// a friendly message instead of a round trip.
const questionSchema = z.discriminatedUnion("type", [
  z.object({
    type: z.literal("boolean"),
    instructions: z.string().min(1).max(4000),
    criteria: z
      .object({ true: z.string().max(1000).optional(), false: z.string().max(1000).optional() })
      .optional(),
  }),
  z.object({
    type: z.literal("choice"),
    instructions: z.string().min(1).max(4000),
    criteria: z.record(z.string().min(1).max(200), z.string().max(1000).nullable()),
  }),
  z.object({
    type: z.literal("score"),
    instructions: z.string().min(1).max(4000),
    criteria: z.array(z.string().min(1).max(500)).min(2).max(10),
  }),
]);

const bodySchema = z.object({
  model: z.string().min(1).max(200),
  state: z.union([z.string().min(1), z.record(z.unknown()), z.array(z.unknown())]),
  questions: z.record(z.string().min(1).max(64), questionSchema),
});

export async function POST(req: NextRequest) {
  const denied = await guardAdmin();
  if (denied) return denied;

  const parsed = bodySchema.safeParse(await req.json().catch(() => null));
  if (!parsed.success) {
    return NextResponse.json({ error: "Invalid verdicts request." }, { status: 400 });
  }
  const { model, state, questions } = parsed.data;
  const ids = Object.keys(questions);
  if (ids.length === 0 || ids.length > 32) {
    return NextResponse.json(
      { error: "A verdicts request needs 1–32 questions." },
      { status: 400 },
    );
  }
  for (const [id, q] of Object.entries(questions)) {
    if (q.type === "choice") {
      const options = Object.keys(q.criteria);
      if (options.length < 2 || options.length > 26) {
        return NextResponse.json(
          { error: `Question "${id}": a choice needs 2–26 options.` },
          { status: 400 },
        );
      }
    }
  }

  const body: GatewayVerdictsBody = { model, state, questions };
  const started = Date.now();
  try {
    const upstream = await gatewayVerdicts(body);
    const latencyMs = Date.now() - started;
    const requestId = upstream.headers.get("x-obleth-request-id");
    const json = await upstream.json().catch(() => null);
    if (!upstream.ok) {
      return NextResponse.json(
        { error: apiErrorMessage(json, "verdict evaluation failed"), requestId },
        { status: upstream.status },
      );
    }
    return NextResponse.json({
      model: json?.model ?? model,
      verdicts: json?.verdicts ?? {},
      usage: json?.usage ?? null,
      latencyMs,
      requestId,
    });
  } catch (e) {
    return NextResponse.json({ error: String(e) }, { status: 502 });
  }
}
