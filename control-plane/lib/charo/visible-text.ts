const THINK_OPEN = /<\s*think\s*>/i;
const THINK_CLOSE = /<\s*\/\s*think\s*>/i;
const ORPHAN_THINK_CLOSE_LIMIT = 2000;

function findTag(text: string, pattern: RegExp): { index: number; end: number } | null {
  const match = pattern.exec(text);
  if (!match || match.index < 0) return null;
  return { index: match.index, end: match.index + match[0].length };
}

/**
 * Some reasoning models emit hidden chain-of-thought in the normal content
 * stream. Charo should show the answer and structured tool cards, not that
 * hidden scratchpad. This strips complete and in-progress <think> blocks.
 */
export function stripHiddenReasoning(text: string): string {
  let rest = text;
  let out = "";

  while (rest) {
    const open = findTag(rest, THINK_OPEN);
    const close = findTag(rest, THINK_CLOSE);

    if (close && (!open || close.index < open.index)) {
      if (!out.trim() && close.index <= ORPHAN_THINK_CLOSE_LIMIT) {
        rest = rest.slice(close.end);
        continue;
      }
      out += rest.slice(0, close.index);
      rest = rest.slice(close.end);
      continue;
    }

    if (!open) {
      out += rest;
      break;
    }

    out += rest.slice(0, open.index);
    rest = rest.slice(open.end);
    const blockClose = findTag(rest, THINK_CLOSE);
    if (!blockClose) break;
    rest = rest.slice(blockClose.end);
  }

  return out.replace(/^\s+/, "");
}
