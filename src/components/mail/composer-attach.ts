/**
 * The gap between letting go of a file and the file being on the draft.
 *
 * Attaching is two round trips through Rust — the draft row, then the bytes —
 * and until this module existed the composer drew nothing at all until both had
 * come back. Dropping a file looked like dropping a file into a hole: no chip,
 * no spinner, no ring, and then some time later a chip. The standing rule is
 * that every action gives immediate feedback and that the user-facing half of
 * it is done inside 200ms, and attaching obeyed neither.
 *
 * Nothing about the round trip is what has to be fast. **The name is already in
 * hand** the instant the drop arrives — it is the last component of the path
 * the webview was handed — so the chip can be drawn from that and reconciled
 * against the real {@link AttachResult} when it lands. What crosses to Rust is
 * unchanged; what changed is that the composer stops waiting to say anything.
 *
 * # What the round trip actually costs
 *
 * Measured against a temporary store, debug build, one file:
 *
 * | file | `saveDraft` | `attachAdd` |
 * |---|---|---|
 * | 120 KB | 1.1ms | 1.4ms |
 * | 2 MB | 0.6ms | 6.9ms |
 * | 8 MB | 0.7ms | 87ms |
 *
 * So the cost is the bytes, and only once they are large: `std::fs::read` of
 * the file and then the same bytes again as a BLOB in SQLite. Base64 is not on
 * this path at all (it is `attachImages`, and only for an image going in the
 * body). Neither is the network — `saveDraft`'s push to Gmail is spawned, not
 * awaited. What the table cannot show, and what makes the real thing worse than
 * this: two IPC crossings rather than one, and a `std::fs::read` of a file
 * macOS has evicted to iCloud, which blocks until it has been downloaded again.
 *
 * # Why the save no longer goes first
 *
 * `ensureSaved` was awaited before the attach because "the store keys the files
 * by draft id — so the draft has to *have* a row before a file can point at
 * it". That turns out not to be true of the store: `compose_attachments` has no
 * foreign key to `compose_drafts`, `attach::add_bytes` never looks the draft
 * up, and `save_draft` never writes to the attachment table, so neither call
 * can spoil the other. They are started together here, which takes one round
 * trip off the wall clock — and the chip is on screen before either.
 */

import type { AttachResult, DraftAttachment } from "@/lib/compose";

/**
 * A file the composer has drawn but the store has not answered for yet.
 *
 * Not a {@link DraftAttachment} with fields left blank: a pending chip has a
 * name and nothing else — no id, no size, no content id — and giving it the
 * same type would let one reach the code that removes files, which would send
 * Rust an id that was never real.
 */
export interface PendingAttachment {
  /** Identity for React and for taking this one down again. Never Rust's id. */
  key: string;
  /** The last component of the dropped path. Rust may still sanitise it. */
  filename: string;
  /** Whether this one was headed for the body, so the chip draws the right icon. */
  inline: boolean;
}

/** Enough variation to tell two drops of the same filename apart. */
let sequence = 0;

/**
 * A chip per dropped path, ready to render.
 *
 * The name is taken off the path rather than asked for, which is the whole
 * point — it is the one piece of the answer that needs no round trip. It can
 * differ from the name that comes back: `safe_filename` rewrites a name with a
 * path separator or a control character in it, and the reconciled chip is the
 * sanitised one, which is also the name the recipient will see.
 */
export function pendingAttachments(paths: readonly string[], inline: boolean): PendingAttachment[] {
  return paths.map((path) => ({
    key: `pending-${(sequence += 1).toString(36)}`,
    filename: filenameOf(path),
    inline,
  }));
}

/** The last component of a path, however it is separated. */
export function filenameOf(path: string): string {
  const parts = path.split(/[\\/]/);
  for (let index = parts.length - 1; index >= 0; index -= 1) {
    if (parts[index] !== "") return parts[index];
  }
  return path;
}

/**
 * The extensions the hint may promise a body drop for.
 *
 * The list `attachments::names::sniff_raster_image` recognises, and nothing
 * else — HEIC is an image everywhere except in that function, so a HEIC dropped
 * on the body is attached and the hint must not have said otherwise.
 *
 * This is a *guess about what is about to happen*, for a label drawn while the
 * file is still in the air and its bytes have not been read. The decision
 * itself is Rust's and is made from the bytes; a `.png` that is really a zip
 * lands as an attachment whatever this said.
 */
const INLINE_EXTENSIONS = new Set(["png", "jpg", "jpeg", "gif", "webp", "bmp", "ico"]);

/** Would dropping these on the body put them in the message? Probably. */
export function looksInlinable(paths: readonly string[]): boolean {
  if (paths.length === 0) return false;
  return paths.every((path) => {
    const name = filenameOf(path).toLowerCase();
    const dot = name.lastIndexOf(".");
    return dot > 0 && INLINE_EXTENSIONS.has(name.slice(dot + 1));
  });
}

/**
 * Everything one attach does, in the order the screen needs it.
 *
 * A function rather than a method on the dock so the sequence can be driven
 * without a DOM: what is being pinned is *when* each callback fires relative to
 * the promise, and that is not a thing a render test can see.
 *
 * The one rule the shape enforces: **a pending chip is never orphaned.** It
 * comes down on the way out of every branch — the files landed, Rust threw, a
 * file was refused — because a chip that is still spinning for a file that was
 * rejected is worse than the lag it was added to cover.
 */
export interface AttachRun {
  paths: readonly string[];
  /** Whether the drop asked for the body. Rust may still say no; see the module note. */
  inline: boolean;
  /** Make sure the draft has a row. Started with the attach, not before it. */
  save: () => Promise<unknown>;
  attach: (paths: readonly string[], inline: boolean) => Promise<AttachResult>;
  /** Draw these now, before anything has been asked of Rust. */
  show: (pending: PendingAttachment[]) => void;
  /** Take them down. Always called, exactly once. */
  settle: (pending: PendingAttachment[]) => void;
  /** The list as the store now has it. */
  applied: (attachments: DraftAttachment[]) => void;
  /** The images that went in the body, so the editor can point at them. */
  placed?: (added: DraftAttachment[]) => Promise<void> | void;
  /** A file that did not make it, by name. Never silent. */
  failed: (message: string) => void;
}

export async function runAttach(run: AttachRun): Promise<void> {
  const pending = pendingAttachments(run.paths, run.inline);
  run.show(pending);

  let result: AttachResult | null = null;
  let error: string | null = null;
  try {
    // Together rather than in sequence — see the module note on why the save
    // does not have to land first.
    [, result] = await Promise.all([run.save(), run.attach(run.paths, run.inline)]);
  } catch (thrown: unknown) {
    error = thrown instanceof Error ? thrown.message : String(thrown);
  }

  // Both state changes in one turn, so the real chips replace the pending ones
  // in a single frame rather than through a gap with neither in it.
  if (result) run.applied(result.attachments);
  run.settle(pending);

  if (error) run.failed(error);
  // A file that was refused must say so by name. Silently attaching three of
  // four is the failure this project has paid most for.
  if (result && result.refused.length > 0) run.failed(result.refused[0]);

  const placed = result?.added.filter((file) => file.inline && file.contentId) ?? [];
  if (placed.length > 0) await run.placed?.(placed);
}
