import { readFileSync } from "node:fs";
import { join } from "node:path";
import { getDb } from "@/lib/db";

let applied = false;

/** Apply the idempotent auth schema once per process. */
export async function applyAuthSchema(): Promise<void> {
  if (applied) return;
  const sql = readFileSync(join(process.cwd(), "db", "auth-schema.sql"), "utf8");
  await getDb().query(sql);
  applied = true;
}
