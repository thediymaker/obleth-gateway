import { describe, expect, it } from "vitest";
import { toWire, type ChatTurn } from "./use-charo-stream";

const PNG = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUg==";
const JPG = "data:image/jpeg;base64,/9j/4AAQ==";

let seq = 0;
const turn = (t: Partial<ChatTurn> & Pick<ChatTurn, "role" | "content">): ChatTurn =>
  ({ id: `t${seq++}`, ...t });

describe("toWire and generated images", () => {
  const withImage = (url = PNG) => [
    turn({ role: "user", content: "draw a cat" }),
    turn({ role: "assistant", content: `Here you go\n\n![a cat](${url})` }),
    turn({ role: "user", content: "now a dog" }),
  ];

  it("strips the payload when the target is not vision-capable", () => {
    const wire = toWire(withImage(), false);
    expect(wire[1].content).toBe("Here you go\n\n[image rendered by generate_image(prompt=\"a cat\") and shown to the user]");
    expect(JSON.stringify(wire)).not.toContain("base64");
  });

  it("strips the payload when the target's capability is unknown", () => {
    expect(JSON.stringify(toWire(withImage()))).not.toContain("base64");
  });

  it("re-attaches the image as a content part for a vision model", () => {
    const wire = toWire(withImage(), true);
    expect(wire[1]).toEqual({
      role: "assistant",
      content: [
        { type: "text", text: "Here you go\n\n[image rendered by generate_image(prompt=\"a cat\") and shown to the user]" },
        { type: "image_url", image_url: { url: PNG } },
      ],
    });
    // The text half still carries no bytes — only the image part does.
    const parts = wire[1].content as Array<{ type: string; text?: string }>;
    expect(parts[0].text).not.toContain("base64");
  });

  it("re-attaches only the most recent generation", () => {
    const wire = toWire(
      [
        turn({ role: "assistant", content: `first\n\n![one](${PNG})` }),
        turn({ role: "user", content: "another" }),
        turn({ role: "assistant", content: `second\n\n![two](${JPG})` }),
      ],
      true,
    );
    expect(wire[0].content).toBe("first\n\n[image rendered by generate_image(prompt=\"one\") and shown to the user]");
    expect(JSON.stringify(wire[2].content)).toContain(JPG);
    expect(JSON.stringify(wire[0])).not.toContain("base64");
  });

  it("keeps a user attachment for a vision model and when capability is unknown", () => {
    const turns = [turn({ role: "user", content: "what is this?", image: PNG })];
    for (const vision of [true, undefined]) {
      expect(toWire(turns, vision)[0].content).toEqual([
        { type: "text", text: "what is this?" },
        { type: "image_url", image_url: { url: PNG } },
      ]);
    }
  });

  it("drops a user attachment a non-multimodal model would reject", () => {
    // Sending it is a 400 ("is not a multimodal model") on every later turn.
    const wire = toWire([turn({ role: "user", content: "what is this?", image: PNG })], false);
    expect(wire[0].content).toBe("what is this?");
  });

  it("still drops errored turns and hidden reasoning", () => {
    const wire = toWire([
      turn({ role: "user", content: "hi" }),
      turn({ role: "assistant", content: "bad", error: "boom" }),
      turn({ role: "assistant", content: "<think>secret</think>answer" }),
    ]);
    expect(wire).toHaveLength(2);
    expect(wire[1].content).toBe("answer");
  });
});
