const THINK_OPEN = /<\s*think\s*>/i;
const THINK_CLOSE = /<\s*\/\s*think\s*>/i;
const ORPHAN_THINK_CLOSE_LIMIT = 2000;

function findTag(text: string, pattern: RegExp): { index: number; end: number } | null {
  const match = pattern.exec(text);
  if (!match || match.index < 0) return null;
  return { index: match.index, end: match.index + match[0].length };
}

/**
 * Some reasoning models emit their chain-of-thought in the normal content
 * stream, inside <think> blocks, instead of the separate `reasoning` field a
 * reasoning parser fills. Split such text into the answer and the thinking:
 * complete blocks, a block still open at the end of a stream, and a leading
 * orphan close (a template that opened <think> itself) all count as thinking.
 */
export function splitHiddenReasoning(text: string): { visible: string; thinking: string } {
  let rest = text;
  let out = "";
  let thinking = "";

  while (rest) {
    const open = findTag(rest, THINK_OPEN);
    const close = findTag(rest, THINK_CLOSE);

    if (close && (!open || close.index < open.index)) {
      if (!out.trim() && close.index <= ORPHAN_THINK_CLOSE_LIMIT) {
        thinking += rest.slice(0, close.index);
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
    if (!blockClose) {
      thinking += rest;
      break;
    }
    thinking += rest.slice(0, blockClose.index);
    rest = rest.slice(blockClose.end);
  }

  return { visible: out.replace(/^\s+/, ""), thinking: thinking.trim() };
}

/**
 * The answer alone: complete and in-progress <think> blocks stripped. Charo
 * shows the answer and structured tool cards, not the scratchpad.
 */
export function stripHiddenReasoning(text: string): string {
  return splitHiddenReasoning(text).visible;
}
