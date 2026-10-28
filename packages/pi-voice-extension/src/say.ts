/**
 * The spoken form of an answer, and how to find it.
 *
 * The written answer stays detailed. When voice mode is on the model is asked to end with one
 * fenced block tagged `say` holding a short conversational summary. That is the "surrounded by
 * certain characters" convention, parsed here (in the extension), not on the phone.
 */

/** The most characters sent to the daemon in one speech message (the daemon caps at 4096 bytes). */
export const SAY_MAX_CHARS = 1000;

export const SAY_INSTRUCTIONS = `

VOICE MODE IS ON. The user is listening, not reading. Keep your written answer exactly as detailed as it would normally be, and END it with one fenced block tagged \`say\` containing a short, natural, spoken summary of what you did or found, in one to three sentences, with no code, file paths, markdown, or lists:

\`\`\`say
Done. I changed two files and the tests pass.
\`\`\`

Write the summary in the user's language. Do not mention this block or that voice mode is on.`;

const COMPLETE_FENCE = /```say[ \t]*\r?\n([\s\S]*?)\r?\n?```/g;
const OPEN_FENCE = /```say[\s\S]*$/;

/** The spoken text of the last complete `say` block in [markdown], cleaned, or null. */
export function extractSay(markdown: string): string | null {
  let last: string | null = null;
  for (const m of markdown.matchAll(COMPLETE_FENCE)) last = m[1] ?? null;
  if (last === null) return null;
  const cleaned = cleanForSpeech(last);
  return cleaned.length > 0 ? cleaned : null;
}

/** [markdown] with every `say` block removed, including one still streaming in (unterminated). */
export function stripSay(markdown: string): string {
  return markdown.replace(COMPLETE_FENCE, "").replace(OPEN_FENCE, "").replace(/\n{3,}/g, "\n\n").trimEnd();
}

/**
 * Plain text for a speech engine. The text is model output, so it is treated as untrusted:
 * control characters are dropped, whitespace collapsed, and the length capped at a sentence or
 * word boundary rather than mid-word.
 */
export function cleanForSpeech(text: string): string {
  // eslint-disable-next-line no-control-regex
  let t = text.replace(/[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f]/g, "").replace(/\s+/g, " ").trim();
  if (t.length > SAY_MAX_CHARS) {
    const cut = t.slice(0, SAY_MAX_CHARS);
    const end = Math.max(cut.lastIndexOf(". "), cut.lastIndexOf("。"), cut.lastIndexOf("! "), cut.lastIndexOf("? "));
    t = end > SAY_MAX_CHARS / 2 ? cut.slice(0, end + 1) : cut.slice(0, Math.max(cut.lastIndexOf(" "), 1));
    t = t.trim();
  }
  return t;
}

/** The text blocks of an assistant message, joined. Thinking and tool calls are not spoken. */
export function assistantText(message: { role?: string; content?: unknown }): string {
  if (message.role !== "assistant" || !Array.isArray(message.content)) return "";
  return message.content
    .filter((b): b is { type: "text"; text: string } => !!b && (b as { type?: string }).type === "text" && typeof (b as { text?: unknown }).text === "string")
    .map((b) => b.text)
    .join("\n");
}
