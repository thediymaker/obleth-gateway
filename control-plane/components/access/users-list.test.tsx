// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { assignUserAction } from "@/app/(dashboard)/users/users-actions";
import { UsersList } from "./users-list";
import type { AdminUser } from "@/lib/auth/users";

vi.mock("@/app/(dashboard)/users/users-actions", () => ({
  assignUserAction: vi.fn(async () => ({ ok: true })),
  setUserStatusAction: vi.fn(async () => ({ ok: true })),
}));
vi.mock("next/navigation", () => ({ useRouter: () => ({ refresh: vi.fn(), push: vi.fn() }) }));

const users: AdminUser[] = [
  { id: "me", email: "jlee@asu.edu", role: "admin", status: "active", tenantId: null },
  { id: "u2", email: "mlee42@asu.edu", role: "user", status: "active", tenantId: "t1" },
  { id: "u3", email: "tbrooks@asu.edu", role: "user", status: "active", tenantId: null },
  { id: "u4", email: "kchen18@asu.edu", role: "user", status: "pending", tenantId: null },
];

let root: Root;
let host: HTMLDivElement;
beforeEach(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
});
afterEach(async () => {
  await act(async () => root.unmount());
  host.remove();
});

describe("users", () => {
  it("puts approvals first and says what each person can reach", async () => {
    await act(async () => root.render(<UsersList users={users} tenants={[{ id: "t1", name: "rcusers" }]} me="me" />));
    const approvals = host.querySelector('[aria-label="Waiting for approval"]')!;
    expect(approvals.textContent).toContain("kchen18@asu.edu");
    const people = host.querySelector('[aria-label="People"]')!;
    expect(people.textContent).toContain("Portal · rcusers");
    expect(people.textContent).toContain("Nothing yet: no tenant");
    expect(people.textContent).not.toContain("kchen18@asu.edu");
    expect(host.textContent).toContain("1 can't reach anything yet");
  });

  it("approves with the chosen role, and Not now leaves them waiting", async () => {
    await act(async () => root.render(<UsersList users={users} tenants={[{ id: "t1", name: "rcusers" }]} me="me" />));
    const approve = [...host.querySelectorAll("button")].find((b) => b.textContent === "Approve")!;
    await act(async () => approve.click());
    const fd = vi.mocked(assignUserAction).mock.calls[0][0];
    expect([fd.get("id"), fd.get("role"), fd.get("tenantId")]).toEqual(["u4", "user", ""]);
    await act(async () => [...host.querySelectorAll("button")].find((b) => b.textContent === "Not now")!.click());
    expect(host.querySelector('[aria-label="Waiting for approval"]')!.textContent).not.toContain("kchen18@asu.edu");
  });
});
