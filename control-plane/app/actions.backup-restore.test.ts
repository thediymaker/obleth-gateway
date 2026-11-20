import { afterEach, describe, expect, it, vi } from "vitest";

afterEach(() => vi.resetModules());

function setup() {
  const restoreBackup = vi.fn(async () => ({ created: {}, updated: {} }));
  vi.doMock("@/lib/auth/roles", () => ({
    requireAdmin: async () => ({ id: "admin-1", email: "admin@example.com", role: "admin", status: "active", tenantId: null }),
  }));
  vi.doMock("next/cache", () => ({ revalidatePath: vi.fn() }));
  vi.doMock("@/lib/obleth", () => ({ obleth: { restoreBackup }, OblethApiError: class extends Error {} }));
  return { restoreBackup };
}

describe("restoreBackupAction", () => {
  it("checks the format and sends the file's own text, not a re-serialised copy", async () => {
    const { restoreBackup } = setup();
    const { restoreBackupAction } = await import("./actions");
    const text = '{\n  "format": "obleth-config-backup",\n  "version": 1,\n  "data": {"keys": []}\n}\n';
    expect(await restoreBackupAction(text)).toMatchObject({ ok: true });
    expect(restoreBackup).toHaveBeenCalledWith(text, { auditActor: "admin@example.com" });
  });

  it("refuses a file that isn't a backup without sending it", async () => {
    const { restoreBackup } = setup();
    const { restoreBackupAction } = await import("./actions");
    expect(await restoreBackupAction("not json")).toEqual({ ok: false, error: "Not a valid JSON file." });
    expect(await restoreBackupAction('{"format":"something-else"}')).toEqual({ ok: false, error: "Not an obleth config backup file." });
    expect(await restoreBackupAction("null")).toEqual({ ok: false, error: "Not an obleth config backup file." });
    expect(restoreBackup).not.toHaveBeenCalled();
  });
});
