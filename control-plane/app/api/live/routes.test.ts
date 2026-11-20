import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

// Every /api/live route goes through liveRoute(), so each one checks for an
// admin and reports failures the same way: the gateway's own status and
// message, never a stringified error.
const routes = readdirSync(__dirname, { recursive: true, encoding: "utf8" })
  .filter((f) => f.endsWith("route.ts"))
  .map((f) => [f, readFileSync(join(__dirname, f), "utf8")] as const);

describe("/api/live routes", () => {
  it("are all found", () => {
    expect(routes.length).toBeGreaterThan(50);
  });

  it.each(routes)("%s goes through liveRoute", (_file, source) => {
    expect(source).toMatch(/export const (GET|POST|PUT|PATCH|DELETE) = liveRoute/);
    expect(source).not.toMatch(/export async function (GET|POST|PUT|PATCH|DELETE)/);
    expect(source).not.toContain("String(e)");
  });
});
