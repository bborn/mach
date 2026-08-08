/**
 * The search language — Gmail's, not one of our own.
 *
 * The whole product thesis is that a Gmail hand should be able to type what it
 * already types. So the operators here are Gmail's spellings, including the
 * ones that are slightly odd (`is:`/`has:`/`in:` overlap, `older_than` with an
 * underscore, `-` for negation rather than `NOT`). Where Gmail has no answer —
 * unterminated quotes, a trailing `-`, three open parens — this parser invents
 * the most forgiving one it can rather than refusing to parse.
 *
 * # Totality
 *
 * `parseSearchQuery` never throws and never returns "invalid". The box is read
 * on every keystroke, so half-typed input is the *normal* case, not the error
 * case: `from:` with nothing after it, `"unfinished`, `subject:(a OR` are all
 * states the user passes through on the way to something meaningful, and each
 * one has to produce a query that can be run. Anything the grammar cannot use
 * degrades into full-text terms or is dropped, never into an exception.
 *
 * # Where the work happens
 *
 * This produces an AST, not SQL. The AST crosses the IPC seam and Rust compiles
 * it against the FTS5 index (`db::queries::compile_search`), because 61k
 * messages is far past the point where filtering in TypeScript is honest. The
 * parser lives here anyway because the query bar has to *show* its
 * interpretation as you type, and a round trip per keystroke to find out what
 * you typed would defeat the point.
 */

/** Operators that take a free-text value. */
export type SearchField = "from" | "to" | "cc" | "bcc" | "subject" | "label" | "filename";

/** Operators that are a state rather than a value. */
export type SearchFlag = "unread" | "read" | "starred" | "attachment";

export type DateBound = "before" | "after";

/**
 * One node of the query.
 *
 * `all` is the identity — what `in:anywhere` and a dropped operator become. It
 * exists so that dropping a meaningless term out of an `OR` cannot silently
 * narrow the query: `x OR in:anywhere` still means everything.
 */
export type SearchNode =
  | { type: "and"; nodes: SearchNode[] }
  | { type: "or"; nodes: SearchNode[] }
  | { type: "not"; node: SearchNode }
  | { type: "all" }
  /** Full text, against the FTS5 index over subject and body. */
  | { type: "text"; value: string; prefix: boolean }
  /**
   * `prefix` is only meaningful for `subject:`, which is answered from the FTS
   * index and can therefore match a half-typed word. The address and label
   * fields are substring matches already.
   */
  | { type: "field"; field: SearchField; value: string; prefix?: boolean }
  | { type: "flag"; flag: SearchFlag }
  /** `before:`/`after:`, already resolved to an absolute epoch millisecond. */
  | { type: "date"; bound: DateBound; ts: number };

export interface ParsedSearch {
  /** Exactly what the user typed, untouched. */
  readonly input: string;
  /** `null` when there is nothing to run — an empty box, or only noise. */
  readonly node: SearchNode | null;
  /**
   * The parse, said back in words: `["from stripe", "unread", "since 1 Feb"]`.
   * This is what the query bar renders, and it is the only feedback that an
   * operator was understood as an operator rather than as a word.
   */
  readonly chips: readonly string[];
  /** Operator-looking things that were read as plain text, e.g. `foo:bar`. */
  readonly unknown: readonly string[];
}

export const EMPTY_SEARCH: ParsedSearch = {
  input: "",
  node: null,
  chips: [],
  unknown: [],
};

export interface ParseOptions {
  /** Injected so relative dates (`newer_than:7d`) are testable. */
  now?: number;
  /**
   * Whether a trailing bare word is treated as a prefix.
   *
   * On by default, because the box is read while it is being typed and `veloci`
   * has to find `velocipede` before the `p`. Turn it off for a query that is
   * known to be finished — a saved search, a test.
   */
  prefixLastTerm?: boolean;
}

/* -------------------------------------------------------------------------- */
/* Tokens                                                                      */
/* -------------------------------------------------------------------------- */

type Token =
  | { kind: "word"; value: string; quoted: boolean; terminated: boolean }
  | { kind: "field"; field: string; value: string; quoted: boolean; terminated: boolean }
  | { kind: "or" }
  | { kind: "and" }
  | { kind: "not" }
  | { kind: "(" }
  | { kind: ")" };

/** The operator names we answer to, lowercased. */
const VALUE_FIELDS = new Set([
  "from",
  "to",
  "cc",
  "bcc",
  "subject",
  "label",
  "filename",
  "in",
  "is",
  "has",
  "before",
  "after",
  "older_than",
  "newer_than",
  "older",
  "newer",
]);

const BREAK = new Set([" ", "\t", "\n", "\r", "(", ")"]);

/**
 * What a `-` cannot attach to. It negates the group it opens (`-(a OR b)`) and
 * a quoted phrase, so only whitespace and a closing paren leave it stranded.
 */
const NEGATION_STOPS = new Set([" ", "\t", "\n", "\r", ")"]);

function tokenize(input: string): Token[] {
  const tokens: Token[] = [];
  let i = 0;

  while (i < input.length) {
    const c = input[i]!;
    if (c === " " || c === "\t" || c === "\n" || c === "\r") {
      i += 1;
      continue;
    }
    if (c === "(") {
      tokens.push({ kind: "(" });
      i += 1;
      continue;
    }
    if (c === ")") {
      tokens.push({ kind: ")" });
      i += 1;
      continue;
    }
    // `-` negates whatever comes next. A lone `-` — mid-keystroke, or someone
    // typing a dash — negates nothing and is dropped rather than swallowing the
    // token after the space.
    if (c === "-" && i + 1 < input.length && !NEGATION_STOPS.has(input[i + 1]!)) {
      tokens.push({ kind: "not" });
      i += 1;
      continue;
    }
    if (c === "|") {
      tokens.push({ kind: "or" });
      i += 1;
      continue;
    }
    if (c === '"') {
      const [value, next, terminated] = readQuoted(input, i);
      tokens.push({ kind: "word", value, quoted: true, terminated });
      i = next;
      continue;
    }

    // A bare run, which may turn out to be `field:value`.
    const start = i;
    while (i < input.length && !BREAK.has(input[i]!) && input[i] !== '"') i += 1;
    const raw = input.slice(start, i);
    const colon = raw.indexOf(":");
    const head = colon > 0 ? raw.slice(0, colon).toLowerCase() : "";

    if (colon > 0 && VALUE_FIELDS.has(head)) {
      // The value may itself be quoted (`subject:"quarterly report"`), in which
      // case the run stopped at the quote and we keep reading.
      const inline = raw.slice(colon + 1);
      if (inline === "" && input[i] === '"') {
        const [value, next, terminated] = readQuoted(input, i);
        tokens.push({ kind: "field", field: head, value, quoted: true, terminated });
        i = next;
      } else {
        tokens.push({ kind: "field", field: head, value: inline, quoted: false, terminated: true });
      }
      continue;
    }

    // Gmail takes bare uppercase OR/AND as operators and lowercase `or` as a
    // word. Following that exactly is the difference between finding mail about
    // Oregon and finding nothing.
    if (raw === "OR") tokens.push({ kind: "or" });
    else if (raw === "AND") tokens.push({ kind: "and" });
    else if (raw === "NOT") tokens.push({ kind: "not" });
    else tokens.push({ kind: "word", value: raw, quoted: false, terminated: true });
  }

  return tokens;
}

/** Reads a `"…"` run starting at `at`. An unterminated quote runs to the end. */
function readQuoted(input: string, at: number): [string, number, boolean] {
  let i = at + 1;
  let value = "";
  while (i < input.length) {
    if (input[i] === '"') return [value, i + 1, true];
    value += input[i];
    i += 1;
  }
  return [value, i, false];
}

/* -------------------------------------------------------------------------- */
/* Grammar                                                                     */
/* -------------------------------------------------------------------------- */

/*
 * expr  := or
 * or    := and (OR and)*
 * and   := unary+
 * unary := NOT unary | primary
 * primary := "(" expr ")" | leaf
 *
 * Every production tolerates running out of input, so an unclosed paren or a
 * dangling OR yields whatever was complete rather than nothing at all.
 */

/**
 * How deep the grammar may nest before the rest of the input is flattened.
 *
 * `"(" * 10000` is a thing a keyboard can produce, and a recursive-descent
 * parser answers it with a stack overflow — which in a React render is a white
 * window, not an error message. Past this depth groups stop being groups and
 * their contents are ANDed in at the top, which is wrong in a way nobody can
 * construct by accident and harmless in the way everybody constructs by
 * accident. Rust caps its side of the tree at the same order of magnitude.
 */
const MAX_PARSE_DEPTH = 24;

interface ParseState {
  tokens: Token[];
  i: number;
  chips: string[];
  unknown: string[];
  now: number;
  prefixLast: boolean;
  /** Index of the last token, so only *that* one gets prefix treatment. */
  lastIndex: number;
}

export function parseSearchQuery(input: string, options: ParseOptions = {}): ParsedSearch {
  const tokens = tokenize(input);
  const state: ParseState = {
    tokens,
    i: 0,
    chips: [],
    unknown: [],
    now: options.now ?? Date.now(),
    prefixLast: options.prefixLastTerm !== false && !/[\s")]$/.test(input),
    lastIndex: tokens.length - 1,
  };

  const nodes: SearchNode[] = [];
  while (state.i < state.tokens.length) {
    const before = state.i;
    const node = parseOr(state, 0);
    if (node) nodes.push(node);
    // A stray `)` parses to nothing; step over it so the loop cannot spin.
    if (state.i === before) state.i += 1;
  }

  const node = combine("and", nodes);
  return {
    input,
    node,
    chips: state.chips,
    unknown: state.unknown,
  };
}

function parseOr(state: ParseState, depth: number): SearchNode | null {
  const branches: SearchNode[] = [];
  const first = parseAnd(state, depth);
  if (first) branches.push(first);
  while (peek(state)?.kind === "or") {
    state.i += 1;
    const next = parseAnd(state, depth);
    if (next) branches.push(next);
  }
  return combine("or", branches);
}

function parseAnd(state: ParseState, depth: number): SearchNode | null {
  const parts: SearchNode[] = [];
  for (;;) {
    const token = peek(state);
    if (!token || token.kind === "or" || token.kind === ")") break;
    if (token.kind === "and") {
      state.i += 1;
      continue;
    }
    const node = parseUnary(state, depth);
    if (node) parts.push(node);
    else break;
  }
  return combine("and", parts);
}

function parseUnary(state: ParseState, depth: number): SearchNode | null {
  const token = peek(state);
  if (!token) return null;
  if (depth > MAX_PARSE_DEPTH) {
    state.i += 1;
    return null;
  }
  if (token.kind === "not") {
    state.i += 1;
    const mark = state.chips.length;
    const inner = parseUnary(state, depth + 1);
    // A negation with nothing to negate is not an error, it is someone who has
    // typed the minus and not yet the word.
    if (!inner) {
      state.chips.length = mark;
      return null;
    }
    // Whatever the inner node said about itself becomes one negated phrase, so
    // `-(urgent OR asap)` reads as one chip rather than two positive ones.
    const inside = state.chips.splice(mark).join(" ");
    state.chips.push(`not ${inside || "anything"}`);
    return { type: "not", node: inner };
  }
  return parsePrimary(state, depth);
}

function parsePrimary(state: ParseState, depth: number): SearchNode | null {
  const token = peek(state);
  if (!token) return null;

  if (token.kind === "(") {
    state.i += 1;
    const inner = parseOr(state, depth + 1);
    if (peek(state)?.kind === ")") state.i += 1;
    return inner;
  }
  if (token.kind === ")") return null;
  if (token.kind === "or" || token.kind === "and" || token.kind === "not") {
    state.i += 1;
    return null;
  }

  const index = state.i;
  state.i += 1;
  return token.kind === "field" ? fieldNode(state, token, index) : textNode(state, token, index);
}

function peek(state: ParseState): Token | undefined {
  return state.tokens[state.i];
}

function combine(type: "and" | "or", nodes: SearchNode[]): SearchNode | null {
  const kept = nodes.filter((n) => n !== null);
  if (kept.length === 0) return null;
  if (kept.length === 1) return kept[0]!;
  return { type, nodes: kept };
}

/* -------------------------------------------------------------------------- */
/* Leaves                                                                      */
/* -------------------------------------------------------------------------- */

function textNode(
  state: ParseState,
  token: Extract<Token, { kind: "word" }>,
  index: number,
): SearchNode | null {
  const value = token.value.trim();
  if (!value) return null;
  /*
   * A quoted phrase is never a prefix — the quotes are the user saying "this
   * exactly". An unterminated quote is the exception: `"quarterly rep` is
   * someone mid-phrase, and matching it as a prefix is what keeps results on
   * screen while they finish typing.
   */
  const prefix = state.prefixLast && index === state.lastIndex && (!token.quoted || !token.terminated);
  state.chips.push(token.quoted ? `“${value}”` : value);
  return { type: "text", value, prefix };
}

const IN_LABELS: Record<string, string> = {
  inbox: "INBOX",
  sent: "SENT",
  draft: "DRAFT",
  drafts: "DRAFT",
  spam: "SPAM",
  trash: "TRASH",
  bin: "TRASH",
  important: "IMPORTANT",
  starred: "STARRED",
  chats: "CHAT",
};

const IS_FLAGS: Record<string, SearchFlag> = {
  unread: "unread",
  read: "read",
  starred: "starred",
};

function fieldNode(
  state: ParseState,
  token: Extract<Token, { kind: "field" }>,
  index: number,
): SearchNode | null {
  const raw = token.value.trim();
  const field = token.field;

  // `from:` with nothing after it is the most common state of a half-typed
  // query. It filters nothing, so it *is* nothing.
  if (!raw) return null;

  switch (field) {
    case "from":
    case "to":
    case "cc":
    case "bcc": {
      const value = normalizeAddress(raw);
      if (!value) return null;
      state.chips.push(`${field} ${value}`);
      return { type: "field", field, value };
    }
    case "subject": {
      const prefix =
        state.prefixLast && index === state.lastIndex && (!token.quoted || !token.terminated);
      state.chips.push(`subject ${raw}`);
      // Subject search rides the FTS index's own `subject` column, so it is a
      // text node in disguise as far as the compiler is concerned — but it is
      // its own field so the compiler can pick the column.
      return { type: "field", field: "subject", value: raw, prefix };
    }
    case "filename": {
      state.chips.push(`file ${raw}`);
      return { type: "field", field: "filename", value: raw };
    }
    case "label": {
      state.chips.push(`label ${raw}`);
      return { type: "field", field: "label", value: raw };
    }
    case "in": {
      const key = raw.toLowerCase();
      if (key === "anywhere" || key === "all" || key === "mail") {
        // Mach searches every label by default, so this asks for what it
        // already does. Kept as `all` rather than dropped: see `SearchNode`.
        state.chips.push("anywhere");
        return { type: "all" };
      }
      if (key === "unread") {
        state.chips.push("unread");
        return { type: "flag", flag: "unread" };
      }
      const labelId = IN_LABELS[key];
      state.chips.push(`in ${key}`);
      // An unknown mailbox is treated as a label name, which is what Gmail
      // does with `in:` for user labels.
      return { type: "field", field: "label", value: labelId ?? raw };
    }
    case "is": {
      const key = raw.toLowerCase();
      const flag = IS_FLAGS[key];
      if (flag) {
        state.chips.push(flag);
        return { type: "flag", flag };
      }
      const labelId = IN_LABELS[key];
      if (labelId) {
        state.chips.push(`in ${key}`);
        return { type: "field", field: "label", value: labelId };
      }
      state.unknown.push(`is:${raw}`);
      return null;
    }
    case "has": {
      const key = raw.toLowerCase();
      if (key === "attachment" || key === "attachments" || key === "file") {
        state.chips.push("has attachment");
        return { type: "flag", flag: "attachment" };
      }
      if (key === "star" || key === "yellow-star" || key === "starred") {
        state.chips.push("starred");
        return { type: "flag", flag: "starred" };
      }
      state.unknown.push(`has:${raw}`);
      return null;
    }
    case "before":
    case "after": {
      const ts = resolveDate(raw, field, state.now);
      if (ts === null) {
        state.unknown.push(`${field}:${raw}`);
        return null;
      }
      state.chips.push(`${field === "before" ? "before" : "since"} ${formatDay(ts)}`);
      return { type: "date", bound: field, ts };
    }
    case "older_than":
    case "older": {
      const ts = relativeMs(raw, state.now);
      if (ts === null) {
        state.unknown.push(`${field}:${raw}`);
        return null;
      }
      state.chips.push(`older than ${raw}`);
      return { type: "date", bound: "before", ts };
    }
    case "newer_than":
    case "newer": {
      const ts = relativeMs(raw, state.now);
      if (ts === null) {
        state.unknown.push(`${field}:${raw}`);
        return null;
      }
      state.chips.push(`newer than ${raw}`);
      return { type: "date", bound: "after", ts };
    }
    default:
      return null;
  }
}

/** `<bob@example.com>` and `Bob <bob@example.com>` are both just the address. */
function normalizeAddress(raw: string): string {
  const angled = /<([^>]+)>/.exec(raw);
  return (angled ? angled[1]! : raw).trim().replace(/^"|"$/g, "").toLowerCase();
}

/* -------------------------------------------------------------------------- */
/* Dates                                                                       */
/* -------------------------------------------------------------------------- */

const RELATIVE = /^(\d+)\s*([dwmy])$/i;

/** Milliseconds in each relative unit. Months and years are the rough ones. */
const UNIT_MS: Record<string, number> = {
  d: 86_400_000,
  w: 7 * 86_400_000,
  m: 30 * 86_400_000,
  y: 365 * 86_400_000,
};

function relativeMs(raw: string, now: number): number | null {
  const match = RELATIVE.exec(raw.trim());
  if (!match) return null;
  const amount = Number(match[1]);
  const unit = UNIT_MS[match[2]!.toLowerCase()];
  if (!Number.isFinite(amount) || unit === undefined) return null;
  return now - amount * unit;
}

/**
 * `before:`/`after:` take a date, and — as a kindness Gmail does not offer —
 * also take the relative forms, because `after:7d` is what everyone types the
 * first time before remembering it is spelled `newer_than`.
 *
 * A bare date means the local midnight that starts that day, for both bounds:
 * `after:2024/01/31` includes everything on the 31st, `before:2024/01/31`
 * excludes it. That is Gmail's rule and it is the only pair that reads right
 * when both are used together.
 */
function resolveDate(raw: string, bound: DateBound, now: number): number | null {
  const relative = relativeMs(raw, now);
  if (relative !== null) return relative;

  const match = /^(\d{4})[/-](\d{1,2})[/-](\d{1,2})$/.exec(raw.trim());
  if (!match) return null;
  const year = Number(match[1]);
  const month = Number(match[2]);
  const day = Number(match[3]);
  if (month < 1 || month > 12 || day < 1 || day > 31) return null;
  const date = new Date(year, month - 1, day, 0, 0, 0, 0);
  if (Number.isNaN(date.getTime())) return null;
  void bound;
  return date.getTime();
}

const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

function formatDay(ts: number): string {
  const date = new Date(ts);
  return `${date.getDate()} ${MONTHS[date.getMonth()]} ${date.getFullYear()}`;
}

/* -------------------------------------------------------------------------- */
/* Small helpers the view needs                                                */
/* -------------------------------------------------------------------------- */

/** True when the query would return the entire mailbox, i.e. filters nothing. */
export function isTrivialSearch(parsed: ParsedSearch): boolean {
  return parsed.node === null || onlyAll(parsed.node);
}

function onlyAll(node: SearchNode): boolean {
  switch (node.type) {
    case "all":
      return true;
    case "and":
    case "or":
      return node.nodes.every(onlyAll);
    case "not":
      return false;
    default:
      return false;
  }
}

/** Every operator Mach understands, for the help affordance under the box. */
export const SEARCH_OPERATORS: readonly { syntax: string; hint: string }[] = [
  { syntax: "from:", hint: "sender name or address" },
  { syntax: "to:", hint: "recipient" },
  { syntax: "cc:", hint: "copied" },
  { syntax: "bcc:", hint: "blind copied" },
  { syntax: "subject:", hint: "words in the subject" },
  { syntax: "label:", hint: "a label, by name" },
  { syntax: "in:", hint: "inbox, sent, trash, spam, anywhere" },
  { syntax: "is:", hint: "unread, read, starred" },
  { syntax: "has:attachment", hint: "carries a file" },
  { syntax: "filename:", hint: "attachment name or extension" },
  { syntax: "before:", hint: "2024/01/31" },
  { syntax: "after:", hint: "2024/01/31" },
  { syntax: "older_than:", hint: "7d, 2m, 1y" },
  { syntax: "newer_than:", hint: "7d, 2m, 1y" },
  { syntax: "OR", hint: "either side" },
  { syntax: "-", hint: "not this" },
  { syntax: '"…"', hint: "exact phrase" },
];

/* -------------------------------------------------------------------------- */
/* Evaluating a query in TypeScript — fixtures only                            */
/* -------------------------------------------------------------------------- */

/**
 * What a thread looks like to the evaluator below.
 *
 * Deliberately structural rather than importing `Thread`: this module is the
 * one piece of search that both halves of the seam share, and giving it a
 * dependency on the UI's row types would drag them across too.
 */
export interface SearchSubject {
  thread: {
    subject: string;
    snippet: string;
    participants: readonly { name: string; email: string }[];
    timestamp: number;
    unread: boolean;
    starred: boolean;
    hasAttachment: boolean;
    labelIds: readonly string[];
  };
  messages: readonly {
    from: { name: string; email: string };
    to: readonly { name: string; email: string }[];
    cc: readonly { name: string; email: string }[];
    subject?: string;
    bodyText: string;
    attachments: readonly { filename: string }[];
  }[];
}

/**
 * Run a query against one thread, in the browser.
 *
 * **This is not how search works.** The app compiles the same AST to SQL and
 * lets SQLite answer it against an FTS5 index, because there are 61k messages
 * and the first thing a row-by-row evaluator would have to do is fetch all of
 * them. This exists for the fixture data source — two dozen threads, no
 * backend, `bun run dev` in a browser tab — so that the parser, the AST and the
 * search view can all be exercised without Tauri. Nothing on the real path
 * calls it.
 */
export function matchesSearchNode(node: SearchNode, subject: SearchSubject): boolean {
  switch (node.type) {
    case "all":
      return true;
    case "and":
      return node.nodes.every((n) => matchesSearchNode(n, subject));
    case "or":
      return node.nodes.some((n) => matchesSearchNode(n, subject));
    case "not":
      return !matchesSearchNode(node.node, subject);
    case "text": {
      const haystack = [
        subject.thread.subject,
        subject.thread.snippet,
        ...subject.messages.map((m) => `${m.subject ?? ""} ${m.bodyText}`),
      ]
        .join(" ")
        .toLowerCase();
      return wordMatch(haystack, node.value, node.prefix);
    }
    case "flag":
      switch (node.flag) {
        case "unread":
          return subject.thread.unread;
        case "read":
          return !subject.thread.unread;
        case "starred":
          return subject.thread.starred;
        case "attachment":
          return subject.thread.hasAttachment;
      }
      return false;
    case "date":
      return node.bound === "before"
        ? subject.thread.timestamp < node.ts
        : subject.thread.timestamp >= node.ts;
    case "field":
      return matchesField(node, subject);
  }
}

function matchesField(
  node: Extract<SearchNode, { type: "field" }>,
  subject: SearchSubject,
): boolean {
  const needle = node.value.toLowerCase();
  switch (node.field) {
    case "from":
      return (
        subject.messages.some((m) => person(m.from).includes(needle)) ||
        subject.thread.participants.some((p) => person(p).includes(needle))
      );
    case "to":
      return subject.messages.some((m) => m.to.some((p) => person(p).includes(needle)));
    case "cc":
      return subject.messages.some((m) => m.cc.some((p) => person(p).includes(needle)));
    case "bcc":
      return false;
    case "subject": {
      const haystack = [subject.thread.subject, ...subject.messages.map((m) => m.subject ?? "")]
        .join(" ")
        .toLowerCase();
      return wordMatch(haystack, node.value, node.prefix === true);
    }
    case "label":
      return subject.thread.labelIds.some((id) => id.toLowerCase() === needle);
    case "filename":
      return subject.messages.some((m) =>
        m.attachments.some((a) => a.filename.toLowerCase().includes(needle)),
      );
  }
}

function person(p: { name: string; email: string }): string {
  return `${p.name} ${p.email}`.toLowerCase();
}

/** Word- or prefix-matching, standing in for what the FTS tokenizer does. */
function wordMatch(haystack: string, term: string, prefix: boolean): boolean {
  const needle = term.toLowerCase().trim();
  if (!needle) return false;
  if (prefix || needle.includes(" ")) return haystack.includes(needle);
  return new RegExp(`\\b${escapeRegExp(needle)}\\b`).test(haystack);
}

function escapeRegExp(value: string): string {
  return value.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}
