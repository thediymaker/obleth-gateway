import type { PlaygroundSession } from "./playground";

export function migrateLegacySessions(stored: PlaygroundSession[], root: string, storage: Pick<Storage, "getItem" | "setItem">) {
      // An old session could contain both modes. Keep both transcripts as
      // separate sessions rather than relabelling a comparison as Charo.
      for (const item of [...stored]) {
        if (item.mode !== "chat") continue;
        const old = storage.getItem(`${root}:${item.id}:chat`);
        let target = "charo";
        if (old) {
          try { target = JSON.parse(old).activeTarget || "charo"; } catch { /* keep assistant */ }
        }
        const hasComparison = !!storage.getItem(`${root}:${item.id}:compare:0`);
        if (hasComparison && old) {
          const chat = { ...item, id: `${item.id}-chat`, title: `${item.title} · chat`, mode: "compare" as const, models: [target] };
          storage.setItem(`${root}:${chat.id}:compare:0`, old);
          stored.push(chat);
        } else if (!hasComparison) {
          item.models = [target];
          if (old) storage.setItem(`${root}:${item.id}:compare:0`, old);
        }
        item.mode = "compare";
      }
}
