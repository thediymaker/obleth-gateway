import { z } from "zod";

export const generationSchema = z.object({
  systemPrompt: z.string().max(32_000).default(""),
  temperature: z.number().min(0).max(2).optional(),
  maxTokens: z.number().int().min(1).max(131_072).optional(),
});
export type GenerationSettings = z.infer<typeof generationSchema>;

const content = z.union([
  z.string(),
  z.array(z.discriminatedUnion("type", [
    z.object({ type: z.literal("text"), text: z.string() }),
    z.object({ type: z.literal("image_url"), image_url: z.object({ url: z.string() }) }),
  ])),
]);

export const chatRequestSchema = z.object({
  model: z.string().trim().min(1).max(512),
  messages: z.array(z.object({ role: z.enum(["system", "user", "assistant", "tool"]), content })).min(1).max(1000),
  bare: z.boolean().optional(),
  generation: generationSchema.optional(),
});
