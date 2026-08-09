/**
 * The little bit of Markdown an assistant actually writes.
 *
 * The drawer used to print whatever came back verbatim, so an answer that said
 * it had written a `**draft**` arrived with the asterisks in it. There were two
 * ways out — strip the markers, or honour them — and honouring them is the one
 * that keeps the meaning: the model marks a word up because that word matters,
 * and a mail client that prints the marks is showing its working.
 *
 * # What it covers, and what it deliberately does not
 *
 * Bold, italic, inline code, headings and bullets. That is what turns up in a
 * few sentences of prose about mail; tables, links, block quotes and fenced
 * code do not, and a parser that handled them would be a dependency and a
 * surface area for the sake of output nobody has seen.
 *
 * Anything unmatched stays literal, which matters more here than in a static
 * document: the drawer renders while the answer is still streaming, so a `**`
 * whose partner has not arrived yet is a normal state, not an error. It reads
 * as asterisks for one frame and resolves itself.
 *
 * Underscores are not emphasis. `_` is a word character in every identifier the
 * agent might mention — `history_id`, `thread_labels` — and italicising the
 * middle of a column name is a worse failure than leaving an underscore alone.
 */

export type Segment =
  | { kind: "text"; text: string }
  | { kind: "strong"; text: string }
  | { kind: "em"; text: string }
  | { kind: "code"; text: string };

export interface MarkdownLine {
  kind: "text" | "heading" | "bullet";
  segments: Segment[];
}

/**
 * Code first, then bold, then italic — the order matters, because the earlier
 * alternatives win a tie and `**x**` must not be read as two empty italics.
 *
 * Emphasis may not open or close on a space, which is what keeps `3 * 4 * 5`
 * as arithmetic rather than as an italicised `4`.
 */
const INLINE =
  /`([^`\n]+)`|\*\*([^\s*](?:[^*]*[^\s*])?)\*\*|\*([^\s*](?:[^*\n]*[^\s*])?)\*/g;

const HEADING = /^ {0,3}(#{1,6})\s+(.*)$/;
const BULLET = /^(\s*)[-*+]\s+(.*)$/;

/** One line of prose, split into runs. Never empty — `[]` means a blank line. */
export function parseInline(text: string): Segment[] {
  const segments: Segment[] = [];
  let last = 0;

  INLINE.lastIndex = 0;
  for (let match = INLINE.exec(text); match !== null; match = INLINE.exec(text)) {
    if (match.index > last) {
      segments.push({ kind: "text", text: text.slice(last, match.index) });
    }
    if (match[1] !== undefined) segments.push({ kind: "code", text: match[1] });
    else if (match[2] !== undefined) segments.push({ kind: "strong", text: match[2] });
    else if (match[3] !== undefined) segments.push({ kind: "em", text: match[3] });
    last = match.index + match[0].length;
  }

  if (last < text.length) segments.push({ kind: "text", text: text.slice(last) });
  return segments;
}

/**
 * The whole answer, line by line.
 *
 * Lines rather than blocks, and blank lines kept as empty ones, because the
 * drawer paints this inside a `whitespace-pre-wrap` block: the layout is the
 * text's own, and this only decides what is emphasis and what is a marker.
 */
export function parseMarkdown(text: string): MarkdownLine[] {
  return text.split("\n").map((line) => {
    const heading = HEADING.exec(line);
    if (heading) return { kind: "heading", segments: parseInline(heading[2] ?? "") };

    const bullet = BULLET.exec(line);
    // A lone `*` is a bullet; `*word*` is emphasis. The second capture being
    // non-empty is the difference, and `parseInline` sees the rest either way.
    if (bullet && (bullet[2] ?? "").length > 0) {
      return {
        kind: "bullet",
        segments: [
          { kind: "text", text: `${bullet[1] ?? ""}• ` },
          ...parseInline(bullet[2] ?? ""),
        ],
      };
    }

    return { kind: "text", segments: parseInline(line) };
  });
}
