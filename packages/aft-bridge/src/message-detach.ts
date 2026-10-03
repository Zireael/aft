/**
 * The rule deciding when an incoming message interrupts a blocking wait
 * (a `wait:true` foreground bash or a sync `bash_watch`), shared by every
 * plugin host so each one detaches and aborts on the same messages.
 *
 * Any new message interrupts while `bash.detach_on_user_message` is on,
 * including machine-generated ones such as a subagent's completion or AFT's own
 * background-completion wake: those are exactly what the agent should act on
 * instead of sitting out the rest of a long wait. With the setting off, only
 * the operator's `&detach` token interrupts.
 */

/** Control token an operator types to detach a wait regardless of config. */
export const BASH_WAIT_DETACH_MAGIC_KEYWORD = "&detach";

const STANDALONE_DETACH_KEYWORD_SOURCE = `(^|[^\\p{L}\\p{N}_])${BASH_WAIT_DETACH_MAGIC_KEYWORD}(?![\\p{L}\\p{N}_])`;
const STANDALONE_DETACH_KEYWORD_PATTERN = new RegExp(STANDALONE_DETACH_KEYWORD_SOURCE, "u");
const STANDALONE_DETACH_KEYWORDS_PATTERN = new RegExp(STANDALONE_DETACH_KEYWORD_SOURCE, "gu");

/** True when the text contains the detach token as a standalone word. */
export function containsStandaloneDetachKeyword(messageText: string): boolean {
  return STANDALONE_DETACH_KEYWORD_PATTERN.test(messageText);
}

/**
 * The `[start, end)` character ranges of the standalone detach tokens that
 * `stripStandaloneDetachKeywords` removes, in order. A host whose message
 * carries offsets into its text (for example mention positions) uses these to
 * move those offsets to where the same characters sit after the strip.
 */
export function standaloneDetachKeywordRanges(messageText: string): Array<[number, number]> {
  const ranges: Array<[number, number]> = [];
  for (const match of messageText.matchAll(STANDALONE_DETACH_KEYWORDS_PATTERN)) {
    // The pattern also consumes the one boundary character before the token,
    // which the strip keeps, so the token starts after that capture.
    const start = (match.index ?? 0) + (match[1]?.length ?? 0);
    ranges.push([start, start + BASH_WAIT_DETACH_MAGIC_KEYWORD.length]);
  }
  return ranges;
}

/** Remove every standalone detach token, keeping the surrounding text. */
export function stripStandaloneDetachKeywords(messageText: string): string {
  return messageText.replace(STANDALONE_DETACH_KEYWORDS_PATTERN, "$1");
}

/** Strip tokens and tidy only the gaps they leave, preserving unrelated whitespace. */
export function stripDetachKeywordsAndTidyGap(messageText: string): string {
  let text = messageText;
  for (const { start, end, length } of detachStripEdits(messageText).reverse()) {
    text = text.slice(0, start) + " ".repeat(length) + text.slice(end);
  }
  return text;
}

/** Collapse only horizontal whitespace joined by removing a token; leave code and tables intact. */
export function detachStripEdits(
  text: string,
): Array<{ start: number; end: number; length: number }> {
  const edits: Array<{ start: number; end: number; length: number }> = [];
  for (const [tokenStart, tokenEnd] of standaloneDetachKeywordRanges(text)) {
    let start = tokenStart;
    let end = tokenEnd;
    const bridgesGap = /[ \t]/.test(text[start - 1] ?? "") && /[ \t]/.test(text[end] ?? "");
    if (bridgesGap) {
      while (start > 0 && /[ \t]/.test(text[start - 1])) start--;
      while (end < text.length && /[ \t]/.test(text[end])) end++;
    }
    const previous = edits.at(-1);
    if (previous && start <= previous.end) {
      previous.end = end;
      previous.length = Math.max(previous.length, bridgesGap ? 1 : 0);
    } else {
      edits.push({ start, end, length: bridgesGap ? 1 : 0 });
    }
  }
  return edits;
}

/**
 * Decide whether a message interrupts the session's blocking waits.
 *
 * `operatorText` is the operator-typed text of the message, taken before the
 * host strips the detach token from it; it is consulted only for that token.
 */
export function shouldInterruptWaitsForMessage(
  detachOnUserMessage: boolean,
  operatorText: string,
): boolean {
  return detachOnUserMessage || containsStandaloneDetachKeyword(operatorText);
}
