// @vitest-environment node
import { beforeEach, describe, expect, it, vi } from "vitest";

// The pages that only name keys (filter pickers, audit names, per-tenant
// counts) ask the gateway for those fields, never the whole key list.
const listKeyFields = vi.fn(async () => []);
const listKeys = vi.fn(async () => []);
vi.mock("@/lib/auth/roles", () => ({ requireAdmin: vi.fn(async () => ({ email: "admin@example.edu" })) }));
vi.mock("@/lib/auth/users", () => ({ listUsers: vi.fn(async () => []) }));
vi.mock("@/lib/obleth", () => ({
  KEY_REJECTION_ROWS: 100,
  obleth: new Proxy({} as Record<string, unknown>, {
    get: (_t, name) => (name === "listKeyFields" ? listKeyFields : name === "listKeys" ? listKeys : vi.fn(async () => [])),
  }),
}));
vi.spyOn(console, "error").mockImplementation(() => {});

const { default: LogsPage } = await import("./logs/page");
const { default: ReportsPage } = await import("./reports/page");
const { default: AuditPage } = await import("./audit/page");
const { default: TenantsPage } = await import("./tenants/page");

beforeEach(() => vi.clearAllMocks());

describe("pages that only name keys", () => {
  it.each([
    ["Request logs", () => LogsPage({ searchParams: Promise.resolve({}) }), ["id", "name", "key_prefix", "tenant_id"]],
    ["Reports", () => ReportsPage({ searchParams: Promise.resolve({}) }), ["id", "name", "key_prefix", "tenant_id"]],
    ["Audit", () => AuditPage({ searchParams: Promise.resolve({}) }), ["id", "name", "key_prefix"]],
    ["Tenants", () => TenantsPage(), ["tenant_id", "disabled"]],
  ])("%s asks for the fields it shows", async (_name, render, fields) => {
    await render();
    expect(listKeyFields).toHaveBeenCalledWith(fields);
    expect(listKeys).not.toHaveBeenCalled();
  });
});
