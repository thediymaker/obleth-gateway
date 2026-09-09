import type { ChatTurn } from "@/components/charo/use-charo-stream";

/** Match broadcasts by id, never by response arrival order or repeated prompt text. */
export function conversationRows(lanes: ChatTurn[][]) {
  const rows = new Map<string, { id: string; prompt?: ChatTurn; answers: Map<number, ChatTurn[]> }>();
  lanes.forEach((messages, lane) => {
    let key = "0:activities";
    let legacy = 0;
    for (const message of messages) {
      if (message.role === "user") key = message.promptId ?? `1:${String(legacy++).padStart(8, "0")}`;
      let row = rows.get(key);
      if (!row) { row = { id: key, answers: new Map() }; rows.set(key, row); }
      if (message.role === "user") row.prompt ??= message;
      else row.answers.set(lane, [...(row.answers.get(lane) ?? []), message]);
    }
  });
  return [...rows.values()].sort((a, b) => a.id.localeCompare(b.id));
}
