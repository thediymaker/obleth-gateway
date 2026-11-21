import { act } from "react";
import type { Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { queryRoot } from "@/components/query-root.test-util";
import { useEnabledModels } from "./use-enabled-models";

let root: Root;
let host: HTMLDivElement;

function Names({ id }: { id: string }) {
  const { models, loading, error } = useEnabledModels();
  return <p id={id}>{loading ? "loading" : error ?? models.map((m) => m.model_name).join(",")}</p>;
}

const flush = () => act(async () => { for (let i = 0; i < 3; i++) await new Promise((r) => setTimeout(r, 0)); });
const text = (id: string) => host.querySelector(`#${id}`)!.textContent;

beforeEach(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  host = document.createElement("div");
  document.body.appendChild(host);
  root = queryRoot(host);
});

afterEach(() => {
  act(() => root.unmount());
  host.remove();
  vi.unstubAllGlobals();
});

describe("useEnabledModels", () => {
  it("two components share one request and see only enabled models", async () => {
    const fetchMock = vi.fn(async () => new Response(JSON.stringify([{ model_name: "on", enabled: true }, { model_name: "off", enabled: false }]), { status: 200 }));
    vi.stubGlobal("fetch", fetchMock);
    act(() => root.render(<><Names id="a" /><Names id="b" /></>));
    await flush();
    expect(fetchMock).toHaveBeenCalledTimes(1);
    expect(fetchMock.mock.calls[0]).toEqual(["/api/live/models", undefined]);
    expect(text("a")).toBe("on");
    expect(text("b")).toBe("on");
  });

  it("a refused read shows the server's own message", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => new Response(JSON.stringify({ error: "The gateway is restarting." }), { status: 409 })));
    act(() => root.render(<Names id="a" />));
    await flush();
    expect(text("a")).toBe("The models couldn't be loaded: The gateway is restarting.");
  });
});
