import { afterEach, describe, expect, it, vi } from "vitest";
afterEach(() => vi.resetModules());

function mockPortalUser() {
  vi.doMock("@/lib/auth/roles", () => ({
    requireUser: async () => ({
      id: "user-A",
      email: "user-a@example.com",
      role: "user",
      status: "active",
      tenantId: "tenant-A",
    }),
  }));
}

it("deletePortalKey refuses a key that belongs to another tenant", async () => {
  mockPortalUser();
  vi.doMock("@/lib/obleth", () => ({
    obleth: {
      listKeys: async (tid: string) => (tid === "tenant-A" ? [{ id: "k-A" }] : []),
      deleteKey: vi.fn(),
    },
  }));
  const { deletePortalKey } = await import("./portal-actions");
  const fd = new FormData(); fd.set("id", "k-OTHER");
  const res = await deletePortalKey(fd);
  expect(res.ok).toBe(false);
});

it("deletePortalKey does NOT call obleth.deleteKey when ownership check fails", async () => {
  const deleteKeyMock = vi.fn();
  mockPortalUser();
  vi.doMock("@/lib/obleth", () => ({
    obleth: {
      listKeys: async (tid: string) => (tid === "tenant-A" ? [{ id: "k-A" }] : []),
      deleteKey: deleteKeyMock,
    },
  }));
  const { deletePortalKey } = await import("./portal-actions");
  const fd = new FormData(); fd.set("id", "k-OTHER");
  await deletePortalKey(fd);
  expect(deleteKeyMock).not.toHaveBeenCalled();
});

it("disablePortalKey refuses a key that belongs to another tenant", async () => {
  const setKeyDisabledMock = vi.fn();
  mockPortalUser();
  vi.doMock("@/lib/obleth", () => ({
    obleth: {
      listKeys: async (tid: string) => (tid === "tenant-A" ? [{ id: "k-A" }] : []),
      setKeyDisabled: setKeyDisabledMock,
    },
  }));
  const { disablePortalKey } = await import("./portal-actions");
  const fd = new FormData(); fd.set("id", "k-OTHER"); fd.set("disabled", "true");
  const res = await disablePortalKey(fd);
  expect(res.ok).toBe(false);
  expect(setKeyDisabledMock).not.toHaveBeenCalled();
});

it("deletePortalKey succeeds for an owned key", async () => {
  mockPortalUser();
  vi.doMock("@/lib/obleth", () => ({
    obleth: {
      listKeys: async (tid: string) => (tid === "tenant-A" ? [{ id: "k-A" }] : []),
      deleteKey: vi.fn().mockResolvedValue(undefined),
    },
  }));
  vi.doMock("next/cache", () => ({ revalidatePath: vi.fn() }));
  const { deletePortalKey } = await import("./portal-actions");
  const fd = new FormData(); fd.set("id", "k-A");
  const res = await deletePortalKey(fd);
  expect(res.ok).toBe(true);
});

it("createPortalKey returns the secret on success", async () => {
  const createKeyMock = vi.fn().mockResolvedValue({ key: { id: "k-new" }, secret: "sk-supersecret" });
  mockPortalUser();
  vi.doMock("@/lib/obleth", () => ({
    obleth: {
      createKey: createKeyMock,
    },
  }));
  vi.doMock("next/cache", () => ({ revalidatePath: vi.fn() }));
  const { createPortalKey } = await import("./portal-actions");
  const fd = new FormData(); fd.set("name", "My Key");
  const res = await createPortalKey(fd);
  expect(res.ok).toBe(true);
  if (res.ok) expect(res.secret).toBe("sk-supersecret");
  expect(createKeyMock).toHaveBeenCalledWith(
    "tenant-A",
    { name: "My Key" },
    { auditActor: "user-a@example.com" },
  );
});

it("createPortalKey rejects a blank name", async () => {
  mockPortalUser();
  vi.doMock("@/lib/obleth", () => ({ obleth: { createKey: vi.fn() } }));
  const { createPortalKey } = await import("./portal-actions");
  const fd = new FormData(); fd.set("name", "   ");
  const res = await createPortalKey(fd);
  expect(res.ok).toBe(false);
});
