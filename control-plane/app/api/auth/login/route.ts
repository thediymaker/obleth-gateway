import { NextResponse } from "next/server";
import { verifyCredentials } from "@/lib/auth/credentials";
import { createSession } from "@/lib/auth/session";

// Simple in-memory fixed-window rate limiter to slow down credential-stuffing
// and brute-force attempts against the dashboard login. The control plane runs
// as a single Node process, so process-local state is sufficient; operators
// fronting it with multiple replicas should add a shared limiter (e.g. Redis).
const MAX_ATTEMPTS = 10;
const WINDOW_MS = 5 * 60 * 1000;
const attempts = new Map<string, { count: number; resetAt: number }>();

function clientKey(req: Request): string {
  const fwd = req.headers.get("x-forwarded-for");
  if (fwd) return fwd.split(",")[0]!.trim();
  return req.headers.get("x-real-ip")?.trim() || "unknown";
}

function rateLimited(key: string): boolean {
  const now = Date.now();
  const entry = attempts.get(key);
  if (!entry || now > entry.resetAt) {
    attempts.set(key, { count: 1, resetAt: now + WINDOW_MS });
    return false;
  }
  entry.count += 1;
  return entry.count > MAX_ATTEMPTS;
}

function recordSuccess(key: string): void {
  attempts.delete(key);
}

export async function POST(req: Request) {
  const key = clientKey(req);
  if (rateLimited(key)) {
    return NextResponse.json(
      { error: "too many attempts, try again later" },
      { status: 429 },
    );
  }

  const body = await req.json().catch(() => null);
  const username = body?.username;
  const password = body?.password;
  if (typeof username !== "string" || typeof password !== "string") {
    return NextResponse.json({ error: "invalid body" }, { status: 400 });
  }
  if (!(await verifyCredentials(username, password))) {
    return NextResponse.json({ error: "unauthorized" }, { status: 401 });
  }
  recordSuccess(key);
  await createSession({ username });
  return NextResponse.json({ ok: true });
}
