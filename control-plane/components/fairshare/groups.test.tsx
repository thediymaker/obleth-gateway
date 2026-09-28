// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { FairshareGroup, FairshareLiveView, Tenant } from "@/lib/obleth";
import { FairshareGroups } from "./groups";

const mocks = vi.hoisted(() => ({ save: vi.fn(), refresh: vi.fn(), invalidate: vi.fn(), view: undefined as FairshareLiveView | undefined }));
vi.mock("@/app/actions", () => ({ setGroupWeightAction: mocks.save }));
vi.mock("next/navigation", () => ({ useRouter: () => ({ refresh: mocks.refresh }) }));
vi.mock("next/link", () => ({ default: ({ href, children, ...rest }: { href: string; children: React.ReactNode }) => <a href={href} {...rest}>{children}</a> }));
vi.mock("@tanstack/react-query", () => ({
  useQueryClient: () => ({ invalidateQueries: mocks.invalidate }),
  useQuery: () => ({ data: mocks.view, isError: false, isFetching: false }),
}));

const group = (name: string, weight: number) => ({ name, weight, created_at: "", updated_at: "" }) as FairshareGroup;
const g = (name: string, weight: number, in_flight: number) => ({ name, weight, in_flight, queued: 0, slot_cap: 0, served_tokens: 0, share_score: 0, weight_share: 0, expected_slots: 0 });
const tenants = [
  { id: "t1", name: "cs-teaching", fairshare_group: "teaching", weight: 100 },
  { id: "t2", name: "library-ai", fairshare_group: "teaching", weight: 50 },
  { id: "t3", name: "research-computing", fairshare_group: "research", weight: 100 },
] as Tenant[];

let host: HTMLDivElement;
let root: Root;
beforeEach(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  mocks.save.mockReset().mockResolvedValue({ ok: true });
  mocks.refresh.mockReset();
  mocks.invalidate.mockReset().mockResolvedValue(undefined);
  mocks.view = { algorithm: "drr", max_in_flight: 64, global_in_flight: 60, global_queued: 0, groups: [g("research", 3, 30), g("teaching", 2, 10), g("default", 1, 20)], tenants: [] } as FairshareLiveView;
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  act(() => root.render(<FairshareGroups groups={[group("research", 3), group("teaching", 2), group("default", 1)]} tenants={tenants} />));
});
afterEach(() => { act(() => root.unmount()); host.remove(); });

async function setWeight(name: string, value: string) {
  const input = host.querySelector<HTMLInputElement>(`[aria-label="${name} weight"]`)!;
  await act(async () => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(input, value);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

describe("FairshareGroups", () => {
  it("shows each group's share of a full pool and what it uses now", () => {
    const text = host.textContent ?? "";
    expect(text).toContain("50%"); // research 3 of 6
    expect(text).toContain("33%"); // teaching 2 of 6
    expect(text).toContain("Borrowing slots others are not using"); // default: 33% used against a 17% share
  });

  it("previews an edit across every group before saving, then saves only what changed", async () => {
    await setWeight("teaching", "3");
    const text = host.textContent ?? "";
    expect(text).toContain("1 change");
    expect(text).toContain("teaching 2 → 3");
    expect(text).toContain("research goes from 50% to 43%");
    expect(text).toContain("default goes from 17% to 14%");
    await act(async () => [...host.querySelectorAll("button")].find((b) => b.textContent === "Save weights")!.click());
    expect(mocks.save).toHaveBeenCalledTimes(1);
    expect(mocks.save).toHaveBeenCalledWith("teaching", 3);
    expect(mocks.refresh).toHaveBeenCalled();
    expect(host.textContent).toContain("Saved 1 weight.");
  });

  it("refuses a weight below 1, and reports a failed save without losing the edit", async () => {
    await setWeight("research", "0");
    expect(host.textContent).toContain("Weights must be whole numbers of at least 1 (research)");
    await setWeight("research", "5");
    mocks.save.mockResolvedValueOnce({ ok: false, error: "gateway unavailable" });
    await act(async () => [...host.querySelectorAll("button")].find((b) => b.textContent === "Save weights")!.click());
    expect(host.querySelector('[role="alert"]')!.textContent).toContain("research: gateway unavailable");
    expect(host.querySelector<HTMLInputElement>('[aria-label="research weight"]')!.value).toBe("5");
  });

  it("lists a group's tenants when it is opened", async () => {
    await act(async () => host.querySelector<HTMLButtonElement>('[aria-label="Show teaching tenants"]')!.click());
    expect(host.textContent).toContain("cs-teaching");
    expect(host.textContent).toContain("library-ai");
    expect(host.textContent).not.toContain("research-computing");
  });
});
