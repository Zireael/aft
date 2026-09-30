/**
 * The hashline rule naming which calls produce an addressable tag.
 *
 * Navigation tools return source that looks edit-ready but never publishes a
 * snapshot, so an agent that inspects a symbol and then patches it is refused
 * for a tag it believes it already has. The rule names the tag sources and the
 * common look-alikes that are not. It is shared by the hashline `edit`
 * description and the workflow hints, and only names tools that exist in the
 * session: the bash rewrite path when AFT's bash is registered with
 * `bash.rewrite` on, and each AFT navigation tool when it is registered.
 * `grep` is always named because disabling AFT's grep leaves the host's own
 * grep under that name, and it does not mint tags either.
 *
 * This module has no imports so the tool modules and the hints can both use it
 * without an import cycle. Kept byte-identical to the OpenCode plugin's copy.
 */
export function hashlineTagSourceSentence(
  bashRewritesMintTags: boolean,
  isRegistered: (toolName: string) => boolean = () => true,
): string {
  const rewrites = bashRewritesMintTags ? " (and accepted AFT `cat`/`head`/`tail` rewrites)" : "";
  const lookAlikes = [
    ...["aft_zoom", "aft_outline"].filter(isRegistered),
    "grep",
    ...(isRegistered("aft_search") ? ["aft_search"] : []),
  ].map((name) => `\`${name}\``);
  lookAlikes.push("conflict snippets");
  return `Only \`read\`${rewrites} mint hashline tags. ${joinList(lookAlikes)} do not. After navigation, call \`read\` on every file and range the patch addresses.`;
}

/** "a", "a and b", or "a, b, and c". */
function joinList(items: string[]): string {
  if (items.length <= 2) return items.join(" and ");
  return `${items.slice(0, -1).join(", ")}, and ${items[items.length - 1]}`;
}
