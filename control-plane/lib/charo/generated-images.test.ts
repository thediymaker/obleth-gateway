import { describe, expect, it } from "vitest";
import { hasGeneratedImage, stripGeneratedImages } from "./generated-images";

const PNG = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUg==";

describe("stripGeneratedImages", () => {
  it("replaces the payload with a placeholder that keeps the alt text", () => {
    const { text, images } = stripGeneratedImages(`Here you go\n\n![a cat in a hat](${PNG})`);
    expect(text).toBe("Here you go\n\n[image rendered by generate_image(prompt=\"a cat in a hat\") and shown to the user]");
    expect(text).not.toContain("base64");
    expect(images).toEqual([PNG]);
  });

  it("collects every attachment in document order", () => {
    const jpg = "data:image/jpeg;base64,/9j/4AAQ==";
    const { text, images } = stripGeneratedImages(`![one](${PNG}) then ![two](${jpg})`);
    expect(text).toBe("[image rendered by generate_image(prompt=\"one\") and shown to the user] then [image rendered by generate_image(prompt=\"two\") and shown to the user]");
    expect(images).toEqual([PNG, jpg]);
  });

  it("keeps an empty alt readable", () => {
    expect(stripGeneratedImages(`![](${PNG})`).text).toBe("[image rendered by the generate_image tool and shown to the user]");
  });

  it("leaves text without an attachment byte-identical", () => {
    const text = "just words, and an ![http image](https://example.test/a.png)";
    const out = stripGeneratedImages(text);
    expect(out.text).toBe(text);
    expect(out.images).toEqual([]);
  });

  it("ignores a non-image data URL", () => {
    const text = "![x](data:text/html;base64,PHNjcmlwdD4=)";
    expect(stripGeneratedImages(text).text).toBe(text);
  });
});

describe("hasGeneratedImage", () => {
  it("is stable across repeated calls on the same regex", () => {
    const text = `![a](${PNG})`;
    // A module-level /g regex carries lastIndex; a second call must not miss.
    expect(hasGeneratedImage(text)).toBe(true);
    expect(hasGeneratedImage(text)).toBe(true);
    expect(hasGeneratedImage("no image here")).toBe(false);
  });
});
