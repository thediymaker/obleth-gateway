import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ImageWorkspace } from "./image-workspace";
import type { PlaygroundSession } from "./playground";
import type { ModelRoute } from "@/lib/obleth";

let root: Root;
let host: HTMLDivElement;
let bodies: unknown[];

const models = [
  { id: "1", model_name: "sdxl", model_type: "image", cost_per_image: 0.01, enabled: true },
  { id: "2", model_name: "llama", model_type: "chat", cost_per_image: 0, enabled: true },
] as unknown as ModelRoute[];

// Mirrors the real key playground.tsx builds: `${root}:${session.id}:image`.
const STORAGE_KEY = "obleth-playground:test:s1:image";

function session(patch: Partial<PlaygroundSession> = {}): PlaygroundSession {
  return {
    id: "s1",
    title: "Untitled session",
    mode: "image",
    models: ["charo"],
    generation: { systemPrompt: "" },
    ...patch,
  };
}

// The themed `Select` opens its option list in a Radix popup on `pointerdown`
// (a plain synthetic `Event` is ignored — Radix reads `button`/`ctrlKey` off
// it), and the options render in a portal on `document.body`, outside `host`.
// Mirrors the helper `settings-form.test.tsx` uses for the same component.
async function openSelect(id: string) {
  await act(async () => {
    host.querySelector<HTMLButtonElement>(`#${id}`)!.dispatchEvent(
      new MouseEvent("pointerdown", { bubbles: true, button: 0 }),
    );
  });
}

function menuItemLabels(): string[] {
  return [...document.querySelectorAll<HTMLElement>("[role='menuitem']")].map((el) => el.textContent ?? "");
}

beforeEach(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  bodies = [];
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  vi.stubGlobal(
    "fetch",
    vi.fn(async (_url: string, init?: RequestInit) => {
      bodies.push(JSON.parse(String(init?.body)));
      return {
        ok: true,
        status: 200,
        json: async () => ({
          images: ["data:image/png;base64,AAAA"],
          latencyMs: 1234,
          requestId: "req-1",
        }),
      } as unknown as Response;
    }),
  );
});

afterEach(() => {
  act(() => root.unmount());
  host.remove();
  vi.unstubAllGlobals();
});

describe("ImageWorkspace", () => {
  it("offers only image-type models as targets", async () => {
    act(() => {
      root.render(
        <ImageWorkspace session={session()} update={() => {}} models={models} loading={false} storageKey={STORAGE_KEY} />,
      );
    });
    await openSelect("image-target");
    const labels = menuItemLabels();
    expect(labels).toContain("sdxl");
    expect(labels).not.toContain("llama");
  });

  it("serialises every parameter, including the non-standard ones", async () => {
    act(() => {
      root.render(
        <ImageWorkspace
          session={session({
            imageModel: "sdxl",
            imagePrompt: "a cat",
            imageNegativePrompt: "blurry",
            imageSize: "1024x1024",
            imageCount: 2,
            imageSteps: 30,
            imageSeed: 7,
          })}
          update={() => {}}
          models={models}
          loading={false}
          storageKey={STORAGE_KEY}
        />,
      );
    });
    const generate = host.querySelector<HTMLButtonElement>("#image-generate")!;
    await act(async () => {
      generate.click();
    });
    expect(bodies).toEqual([
      {
        model: "sdxl",
        prompt: "a cat",
        negative_prompt: "blurry",
        size: "1024x1024",
        n: 2,
        steps: 30,
        seed: 7,
      },
    ]);
  });

  it("omits the optional parameters that were left blank", async () => {
    act(() => {
      root.render(
        <ImageWorkspace
          session={session({ imageModel: "sdxl", imagePrompt: "a cat" })}
          update={() => {}}
          models={models}
          loading={false}
          storageKey={STORAGE_KEY}
        />,
      );
    });
    await act(async () => {
      host.querySelector<HTMLButtonElement>("#image-generate")!.click();
    });
    expect(bodies[0]).toEqual({ model: "sdxl", prompt: "a cat", size: "512x512", n: 1 });
  });

  it("renders the result with its model, latency, and cost", async () => {
    act(() => {
      root.render(
        <ImageWorkspace
          session={session({ imageModel: "sdxl", imagePrompt: "a cat" })}
          update={() => {}}
          models={models}
          loading={false}
          storageKey={STORAGE_KEY}
        />,
      );
    });
    await act(async () => {
      host.querySelector<HTMLButtonElement>("#image-generate")!.click();
    });
    const img = host.querySelector<HTMLImageElement>("img[alt='a cat']")!;
    expect(img.src).toContain("data:image/png;base64,AAAA");
    expect(host.textContent).toContain("sdxl");
    expect(host.textContent).toContain("1234");
    expect(host.textContent).toContain("0.01");
  });

  it("surfaces an upstream failure without clearing the form", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => ({
        ok: false,
        status: 502,
        json: async () => ({ error: "backend unreachable" }),
      }) as unknown as Response),
    );
    act(() => {
      root.render(
        <ImageWorkspace
          session={session({ imageModel: "sdxl", imagePrompt: "a cat" })}
          update={() => {}}
          models={models}
          loading={false}
          storageKey={STORAGE_KEY}
        />,
      );
    });
    await act(async () => {
      host.querySelector<HTMLButtonElement>("#image-generate")!.click();
    });
    expect(host.querySelector("[role='alert']")!.textContent).toContain("backend unreachable");
    expect(host.querySelector<HTMLTextAreaElement>("#image-prompt")!.value).toBe("a cat");
  });

  it("exports the in-memory gallery under exactly the caller's storageKey", async () => {
    act(() => {
      root.render(
        <ImageWorkspace
          session={session({ imageModel: "sdxl", imagePrompt: "a cat" })}
          update={() => {}}
          models={models}
          loading={false}
          storageKey={STORAGE_KEY}
        />,
      );
    });
    await act(async () => {
      host.querySelector<HTMLButtonElement>("#image-generate")!.click();
    });
    const detail: Record<string, unknown> = {};
    act(() => {
      window.dispatchEvent(new CustomEvent("playground-export", { detail }));
    });
    // Asserts the specific key, not "some entry exists" — a wrong or stale key
    // (e.g. the `image-gallery:${session.id}` this replaced) would leave
    // `detail` without this exact property and fail here.
    expect(Object.keys(detail)).toEqual([STORAGE_KEY]);
    expect(detail[STORAGE_KEY]).toMatchObject([{ model: "sdxl", prompt: "a cat" }]);
  });

  it("generates on Enter from the composer", async () => {
    act(() => {
      root.render(
        <ImageWorkspace
          session={session({ imageModel: "sdxl", imagePrompt: "a cat" })}
          update={() => {}}
          models={models}
          loading={false}
          storageKey={STORAGE_KEY}
        />,
      );
    });
    await act(async () => {
      host.querySelector<HTMLTextAreaElement>("#image-prompt")!.dispatchEvent(
        new KeyboardEvent("keydown", { key: "Enter", bubbles: true }),
      );
    });
    expect(bodies).toEqual([{ model: "sdxl", prompt: "a cat", size: "512x512", n: 1 }]);
  });

  // A row records the request that produced it, so Retry reproduces that
  // request. Reading the live toolbar instead would silently re-run an old
  // prompt against new settings and label the result with the old ones.
  it("retries a row with the parameters it was generated with", async () => {
    const render = (imageSize: string) =>
      act(() => {
        root.render(
          <ImageWorkspace
            session={session({ imageModel: "sdxl", imagePrompt: "a cat", imageSize })}
            update={() => {}}
            models={models}
            loading={false}
            storageKey={STORAGE_KEY}
          />,
        );
      });
    render("512x512");
    await act(async () => {
      host.querySelector<HTMLButtonElement>("#image-generate")!.click();
    });
    render("1024x1024");
    await act(async () => {
      host.querySelector<HTMLButtonElement>("[title='Generate this prompt again']")!.click();
    });
    expect(bodies).toHaveLength(2);
    expect(bodies[1]).toMatchObject({ size: "512x512" });
  });

  it("collapses a long backend error behind a disclosure", async () => {
    const long = `litellm.InternalServerError: ${"synStatus 31 ".repeat(20)}`;
    vi.stubGlobal(
      "fetch",
      vi.fn(async () => ({
        ok: false,
        status: 500,
        json: async () => ({ error: long }),
      }) as unknown as Response),
    );
    act(() => {
      root.render(
        <ImageWorkspace
          session={session({ imageModel: "sdxl", imagePrompt: "a cat" })}
          update={() => {}}
          models={models}
          loading={false}
          storageKey={STORAGE_KEY}
        />,
      );
    });
    await act(async () => {
      host.querySelector<HTMLButtonElement>("#image-generate")!.click();
    });
    const alert = host.querySelector("[role='alert']")!;
    // The lead is truncated, but the untruncated text is still reachable.
    expect(alert.querySelector("p")!.textContent).toMatch(/…$/);
    expect(alert.querySelector("summary")!.textContent).toBe("Show details");
    expect(alert.querySelector("pre")!.textContent).toBe(long);
  });

  it("blocks generation with no prompt", () => {
    act(() => {
      root.render(
        <ImageWorkspace
          session={session({ imageModel: "sdxl", imagePrompt: "" })}
          update={() => {}}
          models={models}
          loading={false}
          storageKey={STORAGE_KEY}
        />,
      );
    });
    expect(host.querySelector<HTMLButtonElement>("#image-generate")!.disabled).toBe(true);
  });
});
