import { describe, it, expect } from "vitest";
import { promptFromMessages, imageUrls } from "./images";
import type { ChatMessage } from "./gateway";

describe("promptFromMessages", () => {
  it("uses the latest user turn", () => {
    const messages: ChatMessage[] = [
      { role: "user", content: "a dog" },
      { role: "assistant", content: "" },
      { role: "user", content: "a cat on a hat" },
    ];
    expect(promptFromMessages(messages)).toBe("a cat on a hat");
  });

  it("extracts text parts from multi-part content", () => {
    const messages: ChatMessage[] = [
      {
        role: "user",
        content: [
          { type: "text", text: "a cat" },
          { type: "image_url", image_url: { url: "data:image/png;base64,x" } },
          { type: "text", text: "on a hat" },
        ],
      },
    ];
    expect(promptFromMessages(messages)).toBe("a cat\non a hat");
  });

  it("returns empty when there is no user turn", () => {
    expect(promptFromMessages([{ role: "assistant", content: "hi" }])).toBe("");
    expect(promptFromMessages([])).toBe("");
  });
});

describe("imageUrls", () => {
  it("wraps b64_json items as data URLs", () => {
    expect(imageUrls({ data: [{ b64_json: "abc" }] })).toEqual([
      "data:image/png;base64,abc",
    ]);
  });

  it("passes url items through", () => {
    expect(imageUrls({ data: [{ url: "http://x/img.png" }] })).toEqual([
      "http://x/img.png",
    ]);
  });

  it("handles mixed and malformed items", () => {
    expect(
      imageUrls({ data: [{ b64_json: "a" }, { url: "u" }, {}, null, { b64_json: "" }] }),
    ).toEqual(["data:image/png;base64,a", "u"]);
  });

  it("returns empty for non-object or missing data", () => {
    expect(imageUrls(null)).toEqual([]);
    expect(imageUrls("nope")).toEqual([]);
    expect(imageUrls({ detail: "Not Found" })).toEqual([]);
  });
});
