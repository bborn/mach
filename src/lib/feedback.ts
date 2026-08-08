/**
 * The feedback loop's non-visual half.
 *
 * Three things live here, and none of them renders anything:
 *
 *   1. the ⌘K entry point — a palette resolver, so the palette component keeps
 *      knowing nothing about feedback (`resolver.ts` only puts it in the chain);
 *   2. the two IPC calls, `capture_window` and `submit_feedback`;
 *   3. the annotation state machine, which is a pure reducer so undo and clear
 *      can be tested without a canvas, a pointer, or a DOM.
 *
 * The open/closed state is a two-line store rather than a field on `useMach`:
 * the loop that improves the app should not need a change to the app's own
 * state shape to exist, and a resolver is a plain function with no hooks.
 */

import type { PaletteContext, PaletteResolver, PaletteResult } from "./palette/resolver";
import { isTauri, tauriTransport, toMachError } from "./ipc";

/* -------------------------------------------------------------------------- */
/* Open / closed                                                               */
/* -------------------------------------------------------------------------- */

export interface FeedbackRequest {
  /** Prefilled note text, when ⌘K already had a sentence in it. */
  seed: string;
  /** Bumped on every open so a reopen re-captures. */
  nonce: number;
}

const CLOSED: FeedbackRequest = { seed: "", nonce: 0 };

let current: FeedbackRequest | null = null;
const listeners = new Set<() => void>();

function emit() {
  for (const listener of [...listeners]) listener();
}

export function subscribeFeedback(listener: () => void): () => void {
  listeners.add(listener);
  return () => void listeners.delete(listener);
}

/** The snapshot is referentially stable while nothing changes — `useSyncExternalStore` requires that. */
export function feedbackRequest(): FeedbackRequest | null {
  return current;
}

export function isFeedbackOpen(): boolean {
  return current !== null;
}

export function openFeedback(seed = ""): void {
  current = { seed: seedFromQuery(seed), nonce: (current?.nonce ?? CLOSED.nonce) + 1 };
  emit();
}

export function closeFeedback(): void {
  if (current === null) return;
  current = null;
  emit();
}

/* -------------------------------------------------------------------------- */
/* ⌘K                                                                          */
/* -------------------------------------------------------------------------- */

/**
 * Words that mean "I want to change something", not "find me a message about
 * this". Short and concrete on purpose: a long list would put a command row on
 * top of half the searches he runs.
 */
const TRIGGERS = [
  "feedback",
  "bug",
  "broken",
  "issue",
  "report",
  "fix",
  "annotate",
  "screenshot",
  "suggestion",
  "improve",
  "idea",
  "ugly",
  "cramped",
  "tweak",
  "polish",
  "wrong",
];

const TITLE = "send feedback";

/** A sentence is a request; a word or two is a search. */
const SENTENCE_WORDS = 4;

/**
 * How much this query wants the feedback action. `0` means "not at all".
 *
 * Three ways in, in descending confidence: the action's own name, a trigger
 * word, and — lowest — anything long enough to be a sentence, because "move the
 * account bar to the left" is obviously not a search for mail.
 */
export function feedbackScore(query: string): number {
  const explicit = query.startsWith(">");
  const q = (explicit ? query.slice(1) : query).trim().toLowerCase();

  // `>` with nothing after it lists every command; this is one of them.
  if (!q) return explicit ? 500 : 0;

  if (q.length >= 2 && TITLE.startsWith(q)) return 1000;

  const words = q.split(/\s+/).filter(Boolean);
  for (const word of words) {
    if (word.length < 2) continue;
    for (const trigger of TRIGGERS) {
      if (trigger.startsWith(word)) return 900;
      if (word.startsWith(trigger)) return 850;
    }
  }

  return words.length >= SENTENCE_WORDS ? 300 : 0;
}

/**
 * The note, prefilled from ⌘K when he already typed the request there.
 *
 * Only sentences carry over: seeding the note with the word "feedback" would be
 * one more thing to delete.
 */
export function seedFromQuery(query: string): string {
  const q = (query.startsWith(">") ? query.slice(1) : query).trim();
  if (!q) return "";
  const words = q.split(/\s+/).filter(Boolean);
  if (words.length < SENTENCE_WORDS) return "";
  return q;
}

export const feedbackResolver: PaletteResolver = {
  id: "feedback",
  // Above the ordinary command layer: when he is asking for a change, the way
  // to ask for it belongs at the top.
  priority: 30,
  claims: () => true,
  resolve(ctx: PaletteContext): PaletteResult[] {
    const score = feedbackScore(ctx.query);
    if (score <= 0) return [];
    return [
      {
        id: "command:send-feedback",
        kind: "command",
        title: "Send feedback",
        meta: "screenshot → agent",
        score,
        run: () => openFeedback(ctx.query),
      },
    ];
  },
};

/* -------------------------------------------------------------------------- */
/* IPC                                                                         */
/* -------------------------------------------------------------------------- */

export interface FeedbackContextInfo {
  mode?: string;
  view?: string;
  label?: string;
  account?: string;
  thread?: string;
}

export interface FeedbackReceipt {
  taskId: number | null;
  screenshotPath: string | null;
  message: string;
  output: string;
}

const NEEDS_DESKTOP = "The feedback loop needs the desktop app — this is a browser tab.";

/** The window as a `data:image/png;base64,…` URL, straight from `screencapture`. */
export async function captureWindow(): Promise<string> {
  if (!isTauri()) throw toMachError(NEEDS_DESKTOP);
  try {
    return await tauriTransport.invoke<string>("capture_window");
  } catch (error) {
    throw toMachError(error);
  }
}

export async function submitFeedback(input: {
  text: string;
  imagePngBase64?: string | null;
  context?: FeedbackContextInfo;
}): Promise<FeedbackReceipt> {
  if (!isTauri()) throw toMachError(NEEDS_DESKTOP);
  try {
    const receipt = await tauriTransport.invoke<Partial<FeedbackReceipt>>("submit_feedback", {
      text: input.text,
      imagePngBase64: input.imagePngBase64 ?? null,
      context: input.context ?? null,
    });
    return {
      taskId: typeof receipt?.taskId === "number" ? receipt.taskId : null,
      screenshotPath: receipt?.screenshotPath ?? null,
      message: receipt?.message ?? "Filed.",
      output: receipt?.output ?? "",
    };
  } catch (error) {
    throw toMachError(error);
  }
}

/* -------------------------------------------------------------------------- */
/* Annotation                                                                  */
/* -------------------------------------------------------------------------- */

/**
 * Three tools, no more. An arrow says "*that* one", a box says "this region",
 * a scribble says everything else. A fourth would be a drawing app.
 */
export type AnnotationTool = "pen" | "arrow" | "rect";

export interface Point {
  x: number;
  y: number;
}

/** `pen` keeps every point; `arrow` and `rect` keep exactly start and end. */
export interface Shape {
  tool: AnnotationTool;
  points: Point[];
}

export interface AnnotationState {
  shapes: Shape[];
  /** The stroke under the pointer right now. */
  draft: Shape | null;
}

export type AnnotationAction =
  | { type: "start"; tool: AnnotationTool; point: Point }
  | { type: "move"; point: Point }
  | { type: "commit" }
  | { type: "undo" }
  | { type: "clear" };

export const emptyAnnotation: AnnotationState = { shapes: [], draft: null };

/** Below this the "shape" was a click, not a gesture. In image pixels. */
const MIN_DRAG = 3;

export function annotationReducer(
  state: AnnotationState,
  action: AnnotationAction,
): AnnotationState {
  switch (action.type) {
    case "start":
      return { ...state, draft: { tool: action.tool, points: [action.point] } };

    case "move": {
      const draft = state.draft;
      if (!draft) return state;
      if (draft.tool === "pen") {
        const last = draft.points[draft.points.length - 1];
        // Pointer events fire far faster than the hand moves; dropping the
        // duplicates keeps the exported path small and the line smooth.
        if (last && distance(last, action.point) < 1) return state;
        return { ...state, draft: { ...draft, points: [...draft.points, action.point] } };
      }
      const origin = draft.points[0]!;
      return { ...state, draft: { ...draft, points: [origin, action.point] } };
    }

    case "commit": {
      const draft = state.draft;
      if (!draft) return state;
      // A stray click must not leave an invisible shape that "undo" then has
      // to be pressed twice to get past.
      if (!isDrawn(draft)) return { ...state, draft: null };
      return { shapes: [...state.shapes, draft], draft: null };
    }

    case "undo": {
      if (state.draft) return { ...state, draft: null };
      if (state.shapes.length === 0) return state;
      return { shapes: state.shapes.slice(0, -1), draft: null };
    }

    case "clear":
      return state.shapes.length === 0 && !state.draft ? state : emptyAnnotation;
  }
}

export function isDrawn(shape: Shape): boolean {
  const first = shape.points[0];
  const last = shape.points[shape.points.length - 1];
  if (!first || !last) return false;
  if (shape.tool === "pen") return shape.points.length > 1;
  return distance(first, last) >= MIN_DRAG;
}

/** Is there anything to undo or export? */
export function hasInk(state: AnnotationState): boolean {
  return state.shapes.length > 0 || (state.draft !== null && isDrawn(state.draft));
}

/** Everything currently visible, draft included. */
export function visibleShapes(state: AnnotationState): Shape[] {
  return state.draft && isDrawn(state.draft) ? [...state.shapes, state.draft] : state.shapes;
}

function distance(a: Point, b: Point): number {
  return Math.hypot(a.x - b.x, a.y - b.y);
}

/**
 * Stroke width in image pixels.
 *
 * The capture is retina — 2800px wide for a 1400pt window — so a 2px line would
 * be a hairline nobody can see. Scaling with the image keeps the mark the same
 * apparent weight whatever the window size.
 */
export function strokeWidthFor(imageWidth: number): number {
  return Math.max(2, Math.round(imageWidth / 400));
}
