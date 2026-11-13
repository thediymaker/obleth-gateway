import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { accounts } from "./obleth-accounts";

const ISS = "https://bao.example:8200/v1/identity/oidc/provider/obleth";
const ISS_ENC = encodeURIComponent(ISS);

const fetchMock = vi.fn();

function json(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), { status });
}

function call(i = 0) {
  const [url, init] = fetchMock.mock.calls[i];
  return { url: url as string, init: init as RequestInit & { headers: Record<string, string> } };
}

beforeEach(() => {
  process.env.OBLETH_ADMIN_TOKEN = "tok";
  fetchMock.mockReset();
  vi.stubGlobal("fetch", fetchMock);
});
afterEach(() => vi.unstubAllGlobals());

describe("accounts client", () => {
  it("memberships encodes issuer with slashes and colons", async () => {
    const rows = [{ tenant_id: "u1", tenant_name: "u-s1", kind: "personal", role: "owner" }];
    fetchMock.mockResolvedValue(json(rows));
    expect(await accounts.memberships(ISS, "s 1/x")).toEqual(rows);
    const { url, init } = call();
    expect(url).toMatch(/\/api\/v1\/identities\/memberships\?/);
    const q = new URL(url).searchParams;
    expect(q.get("issuer")).toBe(ISS);
    expect(q.get("subject")).toBe("s 1/x");
    expect(url).toContain(`issuer=${ISS_ENC}`);
    expect(url).not.toContain("https://bao");
    expect(init.method ?? "GET").toBe("GET");
    expect(init.headers.Authorization).toBe("Bearer tok");
  });

  it("provisionIdentity POSTs body", async () => {
    const out = { tenant_id: "t", tenant_name: "u-s1", key_id: "k", tenant_created: true };
    fetchMock.mockResolvedValue(json(out));
    const body = { issuer: ISS, subject: "s1", email: "a@x", rehome: false };
    expect(await accounts.provisionIdentity(body)).toEqual(out);
    const { url, init } = call();
    expect(url.endsWith("/api/v1/identities")).toBe(true);
    expect(init.method).toBe("POST");
    expect(JSON.parse(init.body as string)).toEqual(body);
  });

  it("identitiesByEmail encodes the email", async () => {
    fetchMock.mockResolvedValue(json([]));
    await accounts.identitiesByEmail("a+b@x.com");
    const { url } = call();
    expect(url).toContain("/api/v1/identities?email=a%2Bb%40x.com");
  });

  it("listMembers GETs members", async () => {
    fetchMock.mockResolvedValue(json([]));
    await accounts.listMembers("p1");
    expect(call().url.endsWith("/api/v1/tenants/p1/members")).toBe(true);
  });

  it("upsertMember PUTs with audit actor", async () => {
    const m = { tenant_id: "p1", issuer: ISS, subject: "s1", email: "a@x", role: "member", created_at: "t" };
    fetchMock.mockResolvedValue(json(m));
    const body = { issuer: ISS, subject: "s1", email: "a@x", role: "member" as const };
    expect(await accounts.upsertMember("p1", body, { auditActor: "admin\nx" })).toEqual(m);
    const { url, init } = call();
    expect(url.endsWith("/api/v1/tenants/p1/members")).toBe(true);
    expect(init.method).toBe("PUT");
    expect(JSON.parse(init.body as string)).toEqual(body);
    expect(init.headers["X-Obleth-Audit-Actor"]).toBe("admin x");
  });

  it("removeMember DELETEs with encoded query and accepts 204", async () => {
    fetchMock.mockResolvedValue(new Response(null, { status: 204 }));
    await expect(accounts.removeMember("p1", ISS, "s1", { auditActor: "a" })).resolves.toBeUndefined();
    const { url, init } = call();
    expect(url).toContain(`/api/v1/tenants/p1/members?issuer=${ISS_ENC}&subject=s1`);
    expect(init.method).toBe("DELETE");
    expect(init.headers["X-Obleth-Audit-Actor"]).toBe("a");
  });

  it("listOwnedKeys with and without owner filter", async () => {
    fetchMock.mockImplementation(async () => json([]));
    await accounts.listOwnedKeys("p1", { issuer: ISS, subject: "s1" });
    expect(call(0).url).toContain(`/api/v1/tenants/p1/owned-keys?issuer=${ISS_ENC}&subject=s1`);
    await accounts.listOwnedKeys("p1");
    expect(call(1).url.endsWith("/api/v1/tenants/p1/owned-keys")).toBe(true);
  });

  it("createOwnedKey POSTs", async () => {
    const out = { key: { id: "k", owner_issuer: ISS, owner_subject: "s1" }, secret: "sk_x" };
    fetchMock.mockResolvedValue(json(out));
    const body = { issuer: ISS, subject: "s1", name: "ci" };
    expect(await accounts.createOwnedKey("p1", body)).toEqual(out);
    const { url, init } = call();
    expect(url.endsWith("/api/v1/tenants/p1/owned-keys")).toBe(true);
    expect(init.method).toBe("POST");
    expect(JSON.parse(init.body as string)).toEqual(body);
  });


  it("surfaces the API error message", async () => {
    fetchMock.mockResolvedValue(json({ error: "not a member of this tenant" }, 403));
    await expect(accounts.createOwnedKey("p1", { issuer: "i", subject: "s", name: "n" })).rejects.toThrow(
      "not a member of this tenant",
    );
  });

  it("encodes tenant id path segment", async () => {
    fetchMock.mockResolvedValue(json([]));
    await accounts.listMembers("a/b");
    expect(call().url).toContain("/tenants/a%2Fb/members");
  });
});
