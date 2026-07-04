import { NextResponse } from "next/server";
import { obleth, type ValidateModelBody } from "@/lib/obleth";
import { guardAdmin } from "@/lib/auth/guard";

export async function POST(request: Request) {
  const denied = await guardAdmin();
  if (denied) return denied;
  try {
    const body = (await request.json()) as ValidateModelBody;
    const result = await obleth.validateModel(body);
    return NextResponse.json(result);
  } catch (e) {
    return NextResponse.json({ error: String(e) }, { status: 502 });
  }
}
