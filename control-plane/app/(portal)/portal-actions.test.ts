import { afterEach, describe, expect, it, vi } from "vitest";
afterEach(() => vi.resetModules());

it("deletePortalKey refuses a key that belongs to another tenant", async () => {
  vi.doMock("@/lib/auth/roles", () => ({ requireTenant: async () => "tenant-A" }));
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
  vi.doMock("@/lib/auth/roles", () => ({ requireTenant: async () => "tenant-A" }));
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
  vi.doMock("@/lib/auth/roles", () => ({ requireTenant: async () => "tenant-A" }));
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
  vi.doMock("@/lib/auth/roles", () => ({ requireTenant: async () => "tenant-A" }));
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
  vi.doMock("@/lib/auth/roles", () => ({ requireTenant: async () => "tenant-A" }));
  vi.doMock("@/lib/obleth", () => ({
    obleth: {
      createKey: vi.fn().mockResolvedValue({ key: { id: "k-new" }, secret: "sk-supersecret" }),
    },
  }));
  vi.doMock("next/cache", () => ({ revalidatePath: vi.fn() }));
  const { createPortalKey } = await import("./portal-actions");
  const fd = new FormData(); fd.set("name", "My Key");
  const res = await createPortalKey(fd);
  expect(res.ok).toBe(true);
  if (res.ok) expect(res.secret).toBe("sk-supersecret");
});

it("createPortalKey rejects a blank name", async () => {
  vi.doMock("@/lib/auth/roles", () => ({ requireTenant: async () => "tenant-A" }));
  vi.doMock("@/lib/obleth", () => ({ obleth: { createKey: vi.fn() } }));
  const { createPortalKey } = await import("./portal-actions");
  const fd = new FormData(); fd.set("name", "   ");
  const res = await createPortalKey(fd);
  expect(res.ok).toBe(false);
});
