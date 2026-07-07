import { MessagesSquare } from "lucide-react";
import type { Activity } from "./types";

export const chatWithModelActivity: Activity = {
  id: "chat_with_model",
  label: "Chat with a model",
  blurb: "Talk to a specific model directly",
  icon: MessagesSquare,
  kind: "target",
  steps: [{ type: "model", label: "Model" }],
};
