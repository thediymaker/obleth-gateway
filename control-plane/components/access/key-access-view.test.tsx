import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { KeyAccessView } from "./key-access-view";
import type { KeyRejectionAgg, KeySource } from "@/lib/obleth";

vi.mock("next/link", () => ({ default: ({ href, children, ...rest }: { href: string; children: React.ReactNode }) => <a href={href} {...rest}>{children}</a> }));
const replace = vi.fn();
vi.mock("next/navigation", () => ({ useRouter: () => ({ replace, push: vi.fn() }) }));

const now = Date.now();
const since = now - 86_400_000;
const source = (over: Partial<KeySource> = {}): KeySource => ({
  key_id: "k-1", key_name: "nightly-eval", tenant_id: "t-1", tenant_name: "genomics-lab",
  network: "10.20.0.0/24", client_country: "US", requests: 412, distinct_ips: 2,
  last_seen_ms: now - 5 * 60_000, first_seen_ms: since - 86_400_000, network_first_seen_ms: since - 86_400_000, ...over,
});
const rejection: KeyRejectionAgg = {
  key_id: "k-2", key_name: "old-script", tenant_id: "t-2", tenant_name: "physics", reason: "disabled",
  client_ip: "203.0.113.9", client_country: "DE", count: 37, first_seen_ms: now - 3_600_000, last_seen_ms: now - 60_000, sample_user_agent: "python-requests/2.32",
};
const initial = {
  sinceMs: since,
  sources: [
    source(),
    source({ network: "198.51.100.0/24", client_country: "BR", requests: 3, distinct_ips: 1, first_seen_ms: now - 3_600_000, network_first_seen_ms: now - 3_600_000 }),
    source({ key_id: "k-3", key_name: "ci", tenant_name: "cs-teaching", network: "10.30.0.0/24", requests: 90 }),
    source({ key_id: "k-4", key_name: "lab-box", tenant_name: "cs-teaching", network: "10.139.121.0/24", client_country: "", network_name: "Research computing", requests: 7 }),
    source({ key_id: "k-5", key_name: "vpn-box", tenant_name: "cs-teaching", network: "172.31.20.0/24", client_country: "", requests: 5 }),
  ],
  rejections: [rejection],
};

let root: Root;
let host: HTMLDivElement;

beforeEach(() => {
  vi.stubGlobal("fetch", vi.fn(async () => ({ ok: true, json: async () => initial }) as Response));
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
});

afterEach(() => {
  act(() => root.unmount());
  host.remove();
  vi.unstubAllGlobals();
});

async function render(data = initial) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  await act(async () => {
    root.render(
      <QueryClientProvider client={client}>
        <KeyAccessView tenants={[{ id: "t-1", name: "genomics-lab" }]} initialRange="24h" initialTenant="" initialSearch="" initial={data} />
      </QueryClientProvider>,
    );
  });
}

describe("Key access", () => {
  it("lists each key with its networks and countries, and flags a new country", async () => {
    await render();
    const text = host.textContent ?? "";
    expect(text).toContain("4 keys were used from 5 networks in the last 24 hours.");
    expect(text).toContain("nightly-eval");
    expect(text).toContain("New country");
    expect(text.indexOf("nightly-eval")).toBeLessThan(text.indexOf("ci"));
  });

  it("opens a key to show each network, with links to its requests and the key", async () => {
    await render();
    const row = [...host.querySelectorAll("button[aria-expanded]")].find((b) => b.textContent?.includes("nightly-eval"))!;
    await act(async () => (row as HTMLButtonElement).click());
    expect(host.textContent).toContain("198.51.100.0/24");
    expect(host.textContent).toContain("Brazil");
    const hrefs = [...host.querySelectorAll("a")].map((a) => a.getAttribute("href"));
    expect(hrefs).toContain("/logs?key=k-1&window=24h");
    expect(hrefs).toContain("/keys?key=k-1");
  });

  it("lists refused requests with why, from where and the user agent", async () => {
    await render();
    const text = host.textContent ?? "";
    expect(text).toContain("Key turned off");
    expect(text).toContain("203.0.113.9");
    expect(text).toContain("Germany");
    expect(text).toContain("python-requests/2.32");
  });

  it("pages a long refused list instead of cutting it off", async () => {
    const rejections = Array.from({ length: 250 }, (_, i) => ({ ...rejection, client_ip: `203.0.113.${i}`, count: 250 - i }));
    const data = { ...initial, rejections };
    vi.stubGlobal("fetch", vi.fn(async () => ({ ok: true, json: async () => data }) as Response));
    await render(data);
    const refusedRows = () => host.querySelectorAll("tbody tr").length;
    expect(refusedRows()).toBe(100);
    expect(host.textContent).toContain("100 of 250 refusals shown");
    const more = [...host.querySelectorAll("button")].find((b) => b.textContent === "Show 100 more")!;
    await act(async () => more.click());
    await act(async () => [...host.querySelectorAll("button")].find((b) => b.textContent === "Show 50 more")!.click());
    expect(refusedRows()).toBe(250);
    expect(host.textContent).not.toContain("refusals shown");
  });

  it("searches by network", async () => {
    await render();
    const input = host.querySelector<HTMLInputElement>('input[aria-label="Search keys"]')!;
    await act(async () => {
      const set = Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!;
      set.call(input, "10.20");
      input.dispatchEvent(new Event("input", { bubbles: true }));
    });
    const rows = [...host.querySelectorAll("button[aria-expanded]")].map((b) => b.textContent);
    expect(rows.some((t) => t?.includes("nightly-eval"))).toBe(true);
    expect(rows.some((t) => t?.includes("cs-teaching"))).toBe(false);
  });

  it("names a network from Settings › Networks, and calls an unnamed private one a private address", async () => {
    await render();
    const text = host.textContent ?? "";
    expect(text).toContain("Research computing");
    expect(text).toContain("Private address");
    const hrefs = [...host.querySelectorAll("a")].map((a) => a.getAttribute("href"));
    expect(hrefs).toContain("/settings#networks");
  });

  it("is a tab of Reports and keeps its filters in the Reports address", async () => {
    await render();
    expect(host.querySelector("h1")?.textContent).toBe("Reports");
    const tab = host.querySelector('nav[aria-label="Reports"] a[aria-current="page"]');
    expect(tab?.textContent).toBe("Key access");
    expect(replace).toHaveBeenLastCalledWith("/reports?tab=key-access", { scroll: false });
  });
});
