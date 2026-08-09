import { describe, expect, it } from "vitest";

import { initialUi, overlayOwnsKeyboard, uiReducer } from "./useMach";

/**
 * Starring felt laggy because it had no optimistic path.
 *
 * `bulk(command, hides)` only guessed ahead when `hides` was true — archive and
 * trash, which remove the row. Star passed `false`, so the star did not appear
 * until the whole round trip finished: IPC, the local write, the Gmail call,
 * the threads-changed event, and a refetch.
 *
 * These test the overlay rather than the timing, because the fixture data
 * source resolves instantly and cannot reproduce the delay that made it
 * visible.
 */
describe("optimistic starring", () => {
  it("records the star before anything is confirmed", () => {
    const state = uiReducer(initialUi, { type: "star", threadIds: [1, 2], starred: true });
    expect(state.starOverrides).toEqual({ 1: true, 2: true });
  });

  it("records un-starring too, rather than just forgetting", () => {
    // `{}` would read as "no opinion" and the row would snap back to starred
    // until the refetch landed — the exact flicker being fixed.
    const state = uiReducer(initialUi, { type: "star", threadIds: [7], starred: false });
    expect(state.starOverrides).toEqual({ 7: false });
  });

  it("drops the guess once the command settles", () => {
    const guessed = uiReducer(initialUi, { type: "star", threadIds: [1, 2], starred: true });
    const settled = uiReducer(guessed, { type: "unstar", threadIds: [1] });
    expect(settled.starOverrides).toEqual({ 2: true });
  });

  it("leaves other threads alone", () => {
    const a = uiReducer(initialUi, { type: "star", threadIds: [1], starred: true });
    const b = uiReducer(a, { type: "star", threadIds: [2], starred: false });
    expect(b.starOverrides).toEqual({ 1: true, 2: false });
  });

  it("starts with no opinion at all", () => {
    expect(initialUi.starOverrides).toEqual({});
  });
});

/**
 * The gate every mode consults before it answers a key.
 *
 * Written as a function rather than as `!ui.paletteOpen && !ui.prefsOpen && …`
 * because the list version is what failed: it grew one clause per dialog, and
 * the clause for preferences was never added, so `e` archived a conversation
 * behind it.
 */
describe("who owns the keyboard", () => {
  it("gives it to the app when nothing is on screen", () => {
    expect(overlayOwnsKeyboard(initialUi)).toBe(false);
  });

  it("takes it away for any dialog, not for a list of remembered ones", () => {
    expect(overlayOwnsKeyboard({ ...initialUi, overlays: 1 })).toBe(true);
  });

  it("keeps it while a surface is stacked on another", () => {
    const inner = { ...initialUi, overlays: 2 };
    expect(overlayOwnsKeyboard(inner)).toBe(true);
    // Closing the inner one is not the same as closing both.
    expect(overlayOwnsKeyboard({ ...inner, overlays: 1 })).toBe(true);
  });
});
