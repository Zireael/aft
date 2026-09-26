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

/** Remove every standalone detach token, keeping the surrounding text. */
export function stripStandaloneDetachKeywords(messageText: string): string {
  return messageText.replace(STANDALONE_DETACH_KEYWORDS_PATTERN, "$1");
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
