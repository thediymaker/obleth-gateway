/**
 * The image-generation boon appends its result to the assistant message as
 * `![alt](data:image/png;base64,…)`, up to 3 MB of it. That renders fine, but
 * a conversation replays assistant content on every subsequent turn, and a
 * base64 payload sent back as *text* is both unreadable to the model and
 * enormous: one ~2 MB image measured 570,891 prompt tokens against a 131,072
 * window, so the next turn is a hard 400 and the session is stuck.
 *
 * So the payload comes out of the replayed text. A vision-capable model can be
 * shown the image properly instead, as an `image_url` content part, which the
 * vision encoder charges ~300 tokens for.
 */

/** Matches one attachment, capturing its alt text. */
const ATTACHMENT = /!\[([^\]]*)\]\((data:image\/[A-Za-z0-9.+-]+;base64,[A-Za-z0-9+/=\s]*)\)/g;

/** What the model sees in place of the bytes: that an image exists, and of what. */
function placeholder(alt: string): string {
  const trimmed = alt.trim();
  return trimmed ? `[generated image: ${trimmed}]` : "[generated image]";
}

/**
 * Split assistant text into the version worth sending and the data URLs that
 * were in it, in document order.
 */
export function stripGeneratedImages(text: string): { text: string; images: string[] } {
  const images: string[] = [];
  // `replace` with a global regex walks every match; the callback collects the
  // payloads as a side effect so the text and the images come out of one pass.
  const stripped = text.replace(ATTACHMENT, (_whole, alt: string, url: string) => {
    images.push(url);
    return placeholder(alt);
  });
  return { text: stripped, images };
}

/** True when `text` carries at least one attachment. */
export function hasGeneratedImage(text: string): boolean {
  // A fresh lastIndex each call: the module-level regex is stateful under /g.
  ATTACHMENT.lastIndex = 0;
  return ATTACHMENT.test(text);
}
