import { act } from "react";
import type { Root } from "react-dom/client";
import { queryRoot } from "@/components/query-root.test-util";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { SpeechWorkspace, formatSeconds, formatSpeed } from "./speech-workspace";
import type { PlaygroundSession } from "./playground";
import type { ModelRoute } from "@/lib/obleth";

let root: Root;
let host: HTMLDivElement;
let speechBodies: unknown[];
let voicesReply: () => Response;
let speechReply: () => Response;
let objectUrls: number;

const models = [
  { id: "1", model_name: "tts-a", model_type: "audio_speech", cost_per_character: 0.001, enabled: true },
  { id: "2", model_name: "tts-b", model_type: "audio_speech", cost_per_character: 0, enabled: true },
  { id: "3", model_name: "llama", model_type: "chat", cost_per_character: 0, enabled: true },
] as unknown as ModelRoute[];

// Mirrors the real key playground.tsx builds: `${root}:${session.id}:speech`.
const STORAGE_KEY = "obleth-playground:test:s1:speech";

function session(patch: Partial<PlaygroundSession> = {}): PlaygroundSession {
  return { id: "s1", title: "Untitled session", mode: "speech", models: ["auto"], generation: { systemPrompt: "" }, ...patch };
}

const json = (body: unknown, status = 200) =>
  new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });

/** A WAV header as a streaming server writes it: sizes unknown. */
function streamingHeader(rate = 24_000): Uint8Array<ArrayBuffer> {
  const out = new Uint8Array(44);
  const view = new DataView(out.buffer);
  const text = (at: number, s: string) => [...s].forEach((c, i) => { out[at + i] = c.charCodeAt(0); });
  text(0, "RIFF"); view.setUint32(4, 0xffffffff, true); text(8, "WAVE");
  text(12, "fmt "); view.setUint32(16, 16, true); view.setUint16(20, 1, true); view.setUint16(22, 1, true);
  view.setUint32(24, rate, true); view.setUint32(28, rate * 2, true); view.setUint16(32, 2, true); view.setUint16(34, 16, true);
  text(36, "data"); view.setUint32(40, 0xffffffff, true);
  return out;
}

/** One second of quiet 16-bit mono at `rate`. */
const oneSecond = (rate = 24_000) => new Uint8Array(rate * 2);

function completeWav(rate = 8000): Uint8Array<ArrayBuffer> {
  const data = oneSecond(rate);
  const out = new Uint8Array(44 + data.length);
  out.set(streamingHeader(rate));
  const view = new DataView(out.buffer);
  view.setUint32(4, 36 + data.length, true);
  view.setUint32(40, data.length, true);
  out.set(data, 44);
  return out;
}

const flush = () => act(async () => { for (let i = 0; i < 3; i++) await new Promise((r) => setTimeout(r, 0)); });

function render(s: PlaygroundSession, update: (patch: Partial<PlaygroundSession>) => void = () => {}, list = models) {
  act(() => {
    root.render(<SpeechWorkspace session={s} update={update} models={list} loading={false} storageKey={STORAGE_KEY} />);
  });
}

async function openSelect(id: string) {
  await act(async () => {
    host.querySelector<HTMLButtonElement>(`#${id}`)!.dispatchEvent(new MouseEvent("pointerdown", { bubbles: true, button: 0 }));
  });
}

const menuItems = () => [...document.querySelectorAll<HTMLElement>("[role='menuitem']")];
const readButton = () => host.querySelector<HTMLButtonElement>("#speech-read");
const stopButton = () => host.querySelector<HTMLButtonElement>("#speech-stop");

beforeEach(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  speechBodies = [];
  objectUrls = 0;
  voicesReply = () => json({ voices: ["tara", "leah"] });
  speechReply = () => new Response(completeWav(), { status: 200, headers: { "content-type": "audio/wav" } });
  host = document.createElement("div");
  document.body.appendChild(host);
  root = queryRoot(host);
  // jsdom has no object URLs (and no Web Audio, so nothing plays live here).
  Object.assign(URL, { createObjectURL: vi.fn(() => `blob:test-${++objectUrls}`), revokeObjectURL: vi.fn() });
  vi.stubGlobal(
    "fetch",
    vi.fn(async (url: string, init?: RequestInit) => {
      if (String(url).startsWith("/api/live/playground/voices")) return voicesReply();
      speechBodies.push(JSON.parse(String(init?.body)));
      return speechReply();
    }),
  );
});

afterEach(() => {
  act(() => root.unmount());
  host.remove();
  vi.unstubAllGlobals();
  document.body.innerHTML = "";
});

describe("SpeechWorkspace", () => {
  it("invites pasting text and explains streaming in plain words", async () => {
    render(session());
    await flush();
    expect(host.querySelector("h2")!.textContent).toBe("Paste text to hear it read aloud.");
    expect(host.textContent).toContain("audio starts playing as soon as the first words are ready");
    expect(readButton()!.disabled).toBe(true);
  });

  it("points to the Models page when no text-to-speech model is registered", async () => {
    render(session({ speechText: "Hello" }), () => {}, models.filter((m) => m.model_type === "chat"));
    await flush();
    expect(host.textContent).toContain("No text-to-speech models are registered yet");
    expect(host.querySelector<HTMLAnchorElement>("a[href='/models']")).not.toBeNull();
    expect(readButton()!.disabled).toBe(true);
  });

  it("offers only audio_speech models", async () => {
    render(session());
    await flush();
    await openSelect("speech-target");
    const labels = menuItems().map((el) => el.textContent);
    expect(labels).toEqual(expect.arrayContaining(["tts-a", "tts-b"]));
    expect(labels).not.toContain("llama");
  });

  it("starts a new model on the server's default voice", async () => {
    const update = vi.fn();
    render(session({ speechModel: "tts-a", speechVoice: "tara" }), update);
    await flush();
    await openSelect("speech-target");
    await act(async () => menuItems().find((el) => el.textContent === "tts-b")!.click());
    expect(update).toHaveBeenCalledWith({ speechModel: "tts-b", speechVoice: undefined });
  });

  it("lists the voices the model's server reports, with the server default first", async () => {
    render(session({ speechModel: "tts-a" }));
    await flush();
    expect(vi.mocked(fetch).mock.calls[0][0]).toBe("/api/live/playground/voices?model=tts-a");
    expect(host.querySelector("#speech-voice")!.tagName).toBe("BUTTON");
    await openSelect("speech-voice");
    expect(menuItems().map((el) => el.textContent)).toEqual(["Server default", "tara", "leah"]);
  });

  it("falls back to typing a voice when the server cannot list them", async () => {
    voicesReply = () => json({ voices: [], unsupported: true });
    render(session({ speechModel: "tts-b" }));
    await flush();
    const field = host.querySelector<HTMLInputElement>("#speech-voice")!;
    expect(field.tagName).toBe("INPUT");
    expect(field.placeholder).toBe("Server default");
    expect(host.textContent).toContain("does not list its voices");
    expect(host.querySelector<HTMLInputElement>("#speech-voice-chip")!.tagName).toBe("INPUT");
  });

  it("falls back to typing a voice when the voice list fails to load", async () => {
    voicesReply = () => json({ error: "boom" }, 500);
    render(session({ speechModel: "tts-b" }));
    await flush();
    expect(host.querySelector("#speech-voice")!.tagName).toBe("INPUT");
    expect(host.textContent).toContain("could not be loaded");
  });

  it("sends the trimmed text with the voice and style, leaving blanks out", async () => {
    render(session({ speechModel: "tts-a", speechText: "  Hello there.  ", speechVoice: "tara", speechInstructions: " calm " }));
    await flush();
    await act(async () => readButton()!.click());
    await flush();
    render(session({ speechModel: "tts-b", speechText: "Hi", speechVoice: " ", speechInstructions: "" }));
    await flush();
    await act(async () => readButton()!.click());
    await flush();
    expect(speechBodies).toEqual([
      { model: "tts-a", input: "Hello there.", voice: "tara", instructions: "calm" },
      { model: "tts-b", input: "Hi" },
    ]);
  });

  it("reads aloud with Ctrl+Enter, while Enter alone still makes a new line", async () => {
    render(session({ speechModel: "tts-a", speechText: "Hi" }));
    await flush();
    const textarea = host.querySelector<HTMLTextAreaElement>("#speech-text")!;
    await act(async () => { textarea.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true })); });
    expect(speechBodies).toHaveLength(0);
    await act(async () => { textarea.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", ctrlKey: true, bubbles: true })); });
    await flush();
    expect(speechBodies).toHaveLength(1);
  });

  it("plays a complete file from a backend that does not stream, with its numbers in words", async () => {
    render(session({ speechModel: "tts-a", speechText: "Hello" }));
    await flush();
    await act(async () => readButton()!.click());
    await flush();
    const row = host.querySelector("article")!;
    expect(row.textContent).toContain("Done");
    const details = row.querySelector("[aria-label='Reading details']")!.textContent!;
    expect(details).toMatch(/first audio in \d/);
    expect(details).toContain("1.0 s of audio");
    expect(details).toMatch(/made in \d/);
    expect(details).toMatch(/than real time|as fast as real time/);
    // 5 characters at $0.001 each.
    expect(details).toContain("$0.005");
    expect(row.querySelector<HTMLAudioElement>("audio")!.getAttribute("src")).toBe("blob:test-1");
    expect(row.querySelector<HTMLAnchorElement>("a[download]")!.getAttribute("download")).toMatch(/\.wav$/);
    expect(readButton()).not.toBeNull();
  });

  it("turns Read aloud into Stop while audio streams in, and keeps what arrived when stopped", async () => {
    let push!: ReadableStreamDefaultController<Uint8Array>;
    speechReply = () => new Response(new ReadableStream<Uint8Array>({ start(c) { push = c; } }), { status: 200, headers: { "content-type": "audio/wav" } });
    render(session({ speechModel: "tts-a", speechText: "A long passage." }));
    await flush();
    await act(async () => readButton()!.click());
    await flush();
    expect(readButton()).toBeNull();
    expect(stopButton()).not.toBeNull();
    expect(host.querySelector("article")!.textContent).toContain("Waiting for audio");
    expect(host.querySelector<HTMLButtonElement>("[title='Read this text again']")!.disabled).toBe(true);

    // The header and a first piece arrive split mid-sample.
    const first = new Uint8Array([...streamingHeader(), ...oneSecond().subarray(0, 4801)]);
    await act(async () => { push.enqueue(first); await new Promise((r) => setTimeout(r, 0)); });
    const row = host.querySelector("article")!;
    expect(row.textContent).toContain("Receiving audio");
    expect(row.textContent).toMatch(/first audio in \d/);
    expect(row.textContent).toContain("0.10 s of audio");
    expect(row.querySelector("audio")).toBeNull();

    await act(async () => stopButton()!.click());
    await flush();
    expect(row.textContent).toContain("Stopped");
    expect(row.textContent).not.toContain("made in");
    expect(stopButton()).toBeNull();
    expect(readButton()).not.toBeNull();
    expect(row.querySelector("audio")!.getAttribute("src")).toBe("blob:test-1");
  });

  it("shows the gateway's error on the row without clearing the text", async () => {
    speechReply = () => json({ error: "backend unreachable" }, 502);
    render(session({ speechModel: "tts-a", speechText: "Hello" }));
    await flush();
    await act(async () => readButton()!.click());
    await flush();
    expect(host.querySelector("[role='alert']")!.textContent).toContain("backend unreachable");
    expect(host.querySelector("article")!.textContent).toContain("Error");
    expect(host.querySelector("article audio")).toBeNull();
    expect(host.querySelector<HTMLTextAreaElement>("#speech-text")!.value).toBe("Hello");
    expect(readButton()!.disabled).toBe(false);
  });

  it("reports a reply that is not audio at all", async () => {
    speechReply = () => new Response("", { status: 200, headers: { "content-type": "audio/wav" } });
    render(session({ speechModel: "tts-a", speechText: "Hello" }));
    await flush();
    await act(async () => readButton()!.click());
    await flush();
    expect(host.querySelector("[role='alert']")!.textContent).toContain("returned no audio");
  });

  it("keeps a file in another format for replay when the backend ignored the request for WAV", async () => {
    speechReply = () => new Response(new Uint8Array([0x49, 0x44, 0x33, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]), { status: 200, headers: { "content-type": "audio/mpeg" } });
    render(session({ speechModel: "tts-a", speechText: "Hello" }));
    await flush();
    await act(async () => readButton()!.click());
    await flush();
    const row = host.querySelector("article")!;
    expect(row.textContent).toContain("Done");
    expect(row.querySelector("audio")).not.toBeNull();
    expect(row.querySelector<HTMLAnchorElement>("a[download]")!.getAttribute("download")).toMatch(/\.mp3$/);
  });

  it("clamps long text with a Show all toggle", async () => {
    render(session({ speechModel: "tts-a", speechText: "word ".repeat(100).trim() }));
    await flush();
    await act(async () => readButton()!.click());
    await flush();
    const toggle = [...host.querySelectorAll("article button")].find((b) => b.textContent === "Show all")!;
    expect(host.querySelector("article p")!.className).toContain("line-clamp-4");
    await act(async () => (toggle as HTMLButtonElement).click());
    expect(host.querySelector("article p")!.className).not.toContain("line-clamp-4");
    expect(toggle.textContent).toBe("Show less");
  });

  it("exports run details under exactly the caller's storageKey, without the audio", async () => {
    render(session({ speechModel: "tts-a", speechText: "Hello", speechVoice: "tara" }));
    await flush();
    await act(async () => readButton()!.click());
    await flush();
    const detail: Record<string, unknown> = {};
    act(() => { window.dispatchEvent(new CustomEvent("playground-export", { detail })); });
    expect(Object.keys(detail)).toEqual([STORAGE_KEY]);
    const [run] = detail[STORAGE_KEY] as Record<string, unknown>[];
    expect(run).toMatchObject({ text: "Hello", model: "tts-a", voice: "tara", status: "done", audioSeconds: 1 });
    expect(JSON.stringify(run)).not.toContain("blob:");
  });

  it("frees the audio it held on Clear and on unmount", async () => {
    render(session({ speechModel: "tts-a", speechText: "Hello" }));
    await flush();
    await act(async () => readButton()!.click());
    await flush();
    await act(async () => readButton()!.click());
    await flush();
    const clear = [...host.querySelectorAll("button")].find((b) => b.textContent === "Clear results")!;
    await act(async () => clear.click());
    expect(vi.mocked(URL.revokeObjectURL).mock.calls.map((c) => c[0])).toEqual(["blob:test-1", "blob:test-2"]);
    expect(host.querySelector("article")).toBeNull();

    await act(async () => readButton()!.click());
    await flush();
    act(() => root.unmount());
    expect(vi.mocked(URL.revokeObjectURL)).toHaveBeenLastCalledWith("blob:test-3");
    root = queryRoot(host);
  });
});

describe("speech numbers in words", () => {
  it("formats durations the way people say them", () => {
    expect(formatSeconds(0.42)).toBe("0.42 s");
    expect(formatSeconds(6.08)).toBe("6.1 s");
    expect(formatSeconds(192.4)).toBe("3 min 12 s");
  });

  it("says faster or slower than real time", () => {
    expect(formatSpeed(2)).toBe("2.0× faster than real time");
    expect(formatSpeed(1)).toBe("about as fast as real time");
    expect(formatSpeed(0.5)).toBe("2.0× slower than real time");
  });
});
