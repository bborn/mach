import { afterAll, beforeEach, describe, expect, it } from "vitest";
import {
  closeHandoff,
  decodeChunk,
  describeTarget,
  draftTarget,
  handoffRequest,
  handoffResolver,
  handoffScore,
  nameFromDir,
  noteFromQuery,
  openHandoff,
  rankTargets,
  activeSessionId,
  selectSession,
  sessionsSnapshot,
  setSessions,
  setTargets,
  stepSession,
  subscribeSession,
  targetProblem,
  terminalFromSelection,
  terminalItems,
  terminalSelection,
  unbalanced,
  OTHER_TERMINAL,
  SYSTEM_TERMINAL,
  type HandoffTarget,
  type InstalledTerminal,
} from "./handoff";
import { registerResolver, resolve, type PaletteContext } from "./palette/resolver";

/** The palette's context with nothing in it: only the resolver chain is under test. */
function context(query: string): PaletteContext {
  return {
    query,
    threads: [],
    events: [],
    people: [],
    mailboxes: [],
    commands: [],
    actions: {
      openThread: () => {},
      openEvent: () => {},
      openMailbox: () => {},
      runCommand: () => {},
      composeTo: () => {},
    },
  };
}

function target(patch: Partial<HandoffTarget> = {}): HandoffTarget {
  return {
    id: "t1",
    name: "OfferLab",
    dir: "~/Projects/offerlab",
    run: 'claude "{{prompt}}"',
    mode: "terminal",
    lastRunAt: null,
    ...patch,
  };
}

// The component registers this on mount; the tests do it once so that the seam
// itself — `registerResolver`, not a direct call — is what is under test.
const unregister = registerResolver(handoffResolver);
afterAll(unregister);

/* -------------------------------------------------------------------------- */
/* ⌘K                                                                          */
/* -------------------------------------------------------------------------- */

describe("the ⌘K entry point", () => {
  beforeEach(() => {
    closeHandoff();
    setTargets([target()]);
  });

  it("offers a row per target for a sentence, which is never a mail search", () => {
    const rows = resolve(context("implement this feature request from Katie")).filter((r) =>
      r.id.startsWith("command:handoff:"),
    );
    expect(rows).toHaveLength(1);
    expect(rows[0]?.title).toBe("Hand off to OfferLab");
    expect(rows[0]?.meta).toBe("claude · terminal");
  });

  it("stays out of the way of an ordinary search", () => {
    for (const query of ["katie", "invoice", "re: q3"]) {
      expect(resolve(context(query)).filter((r) => r.id.startsWith("command:handoff")), query)
        .toHaveLength(0);
    }
  });

  it("surfaces on the words that mean “take this elsewhere”", () => {
    for (const query of ["hand off", "handoff", "hand", "send to"]) {
      expect(handoffScore(query), query).toBeGreaterThan(500);
    }
  });

  it("puts the target he named first", () => {
    setTargets([target(), target({ id: "t2", name: "Mach" })]);
    const rows = rankTargets("fix the palette in mach please", [target(), target({ id: "t2", name: "Mach" })]);
    expect(rows[0]?.name).toBe("Mach");
  });

  it("does not match a target on a name too short to mean anything", () => {
    const short = target({ id: "t3", name: "ty" });
    // "ty" appears inside "quickly"; a two-letter name must not claim it.
    expect(rankTargets("reply quickly to katie about the thing", [target(), short])[0]?.name).toBe(
      "OfferLab",
    );
  });

  it("offers to set one up when nothing is configured yet", () => {
    setTargets([]);
    const rows = resolve(context("implement this feature request from Katie"));
    expect(rows.find((r) => r.id === "command:handoff-setup")).toBeDefined();
  });

  it("reaches the editor by name", () => {
    const row = resolve(context("handoff tar")).find((r) => r.id === "command:handoff-targets");
    expect(row).toBeDefined();
    expect(row?.meta).toBe("1 target");
  });

  it("carries the sentence he typed into the request", () => {
    const row = resolve(context("reschedule next week's standups")).find((r) =>
      r.id.startsWith("command:handoff:"),
    );
    row?.run();
    const request = handoffRequest();
    expect(request?.kind).toBe("run");
    if (request?.kind === "run") {
      expect(request.note).toBe("reschedule next week's standups");
      expect(request.targetId).toBe("t1");
    }
  });

  it("does not seed the note from a query that was only a keyword", () => {
    expect(noteFromQuery("handoff")).toBe("");
    expect(noteFromQuery("> hand off")).toBe("");
    expect(noteFromQuery("draft a reply to this thread")).toBe("draft a reply to this thread");
  });

  it("lists every target in > mode with nothing typed", () => {
    expect(handoffScore(">")).toBe(500);
    expect(handoffScore("")).toBe(0);
  });
});

/* -------------------------------------------------------------------------- */
/* Opening                                                                     */
/* -------------------------------------------------------------------------- */

describe("the open/closed store", () => {
  beforeEach(() => closeHandoff());

  it("bumps the nonce on every open so the same target can be used twice", () => {
    openHandoff("t1", "do the thing");
    const first = handoffRequest();
    openHandoff("t1", "do the thing");
    const second = handoffRequest();
    expect(second?.nonce).toBeGreaterThan(first?.nonce ?? 0);
  });

  it("closes to nothing at all", () => {
    openHandoff("t1", "x");
    closeHandoff();
    expect(handoffRequest()).toBeNull();
  });
});

/* -------------------------------------------------------------------------- */
/* Targets                                                                     */
/* -------------------------------------------------------------------------- */

describe("a target", () => {
  it("is named after the directory it was seeded from", () => {
    expect(nameFromDir("~/Projects/offerlab")).toBe("offerlab");
    expect(nameFromDir("/Users/x/mach/")).toBe("mach");
    expect(nameFromDir("/")).toBe("Handoff");
    expect(draftTarget("/Users/x/mach").name).toBe("mach");
  });

  it("starts as a claude session in terminal mode, because that is the useful default", () => {
    const seed = draftTarget("/Users/x/mach");
    expect(seed.run).toBe('claude "{{prompt}}"');
    expect(seed.mode).toBe("terminal");
    expect(targetProblem(seed)).toBeNull();
  });

  it("reads as its program and its mode", () => {
    expect(describeTarget(target({ run: "ty task create {{note}}", mode: "inline" }))).toBe(
      "ty · inline",
    );
  });

  it("says what is wrong while he is still typing", () => {
    expect(targetProblem(target({ name: "  " }))).toBe("Name it");
    expect(targetProblem(target({ dir: "" }))).toBe("Give it a directory");
    expect(targetProblem(target({ run: "" }))).toBe("Give it a command");
    expect(targetProblem(target({ run: 'claude "{{prompt}}' }))).toBe("Unclosed quote");
    expect(targetProblem(target({ run: "FOO=bar claude" }))).toContain("assignment");
    expect(targetProblem(target({ run: "{{prompt}}" }))).toContain("placeholder");
  });

  it("does not mistake an apostrophe inside double quotes for an open quote", () => {
    // The single most annoying possible false positive: `don't` is ordinary
    // English and the field would light up red while he typed the sentence.
    expect(unbalanced(`claude "don't break this"`)).toBe(false);
    expect(unbalanced(`claude 'a "b" c'`)).toBe(false);
    expect(unbalanced(`claude "unterminated`)).toBe(true);
    expect(unbalanced(`claude \\" fine`)).toBe(false);
  });
});

describe("the terminal a handoff opens in", () => {
  const installed: InstalledTerminal[] = [
    { name: "Terminal", path: "/System/Applications/Utilities/Terminal.app" },
    { name: "iTerm", path: "/Applications/iTerm.app" },
  ];

  it("offers the system's, then what is installed, then a way to name one", () => {
    expect(terminalItems(installed).map((item) => item.label)).toEqual([
      "System default",
      "Terminal",
      "iTerm",
      "Other…",
    ]);
    // A Mac with nothing detectable still offers both ends of the menu.
    expect(terminalItems([]).map((item) => item.value)).toEqual([SYSTEM_TERMINAL, OTHER_TERMINAL]);
  });

  it("selects the system's when nothing is stored", () => {
    expect(terminalSelection("", installed)).toBe(SYSTEM_TERMINAL);
    expect(terminalSelection("   ", installed)).toBe(SYSTEM_TERMINAL);
  });

  it("selects an installed terminal by its own name", () => {
    expect(terminalSelection("iTerm", installed)).toBe("iTerm");
  });

  it("keeps a name it did not detect in the text field rather than dropping it", () => {
    // The escape hatch, and the application that has been uninstalled since it
    // was chosen. Both keep their text on screen: a value that will fail at
    // launch has to be visible in the control that set it.
    expect(terminalSelection("/Users/x/Applications/Ghostty.app", installed)).toBe(OTHER_TERMINAL);
    expect(terminalSelection("Warp", installed)).toBe(OTHER_TERMINAL);
  });

  it("stores the empty string for the system default and the name for anything else", () => {
    expect(terminalFromSelection(SYSTEM_TERMINAL, "iTerm")).toBe("");
    expect(terminalFromSelection("iTerm", "")).toBe("iTerm");
    // "Other" opens a field; what is already there stays there to be edited.
    expect(terminalFromSelection(OTHER_TERMINAL, "iTerm")).toBe("iTerm");
    expect(terminalFromSelection(OTHER_TERMINAL, "")).toBe("");
  });
});

/**
 * The session pane's half of this module.
 *
 * The pty, the reaping and the flood cap are Rust's, and
 * `src-tauri/tests/handoff_session.rs` drives those against real processes.
 * What is testable here is the wire: the store the dialog writes and the pane
 * reads, and the decode that turns a chunk back into bytes.
 */
describe("the session store", () => {
  beforeEach(() => setSessions([]));
  afterAll(() => setSessions([]));

  function session(id: string, name = "Mach") {
    return {
      sessionId: id,
      targetName: name,
      command: `claude "fix the calendar header"`,
      dir: "/Users/x/Projects/mach",
      prompt: "fix the calendar header",
      contextFile: "/tmp/mach-handoff-abc/context.txt",
      resources: [] as string[],
    };
  }

  it("holds the tabs and tells its subscribers", () => {
    const seen: (string | null)[] = [];
    const unsubscribe = subscribeSession(() => seen.push(activeSessionId()));

    setSessions([session("s1")], "s1");
    expect(sessionsSnapshot()).toHaveLength(1);
    expect(activeSessionId()).toBe("s1");

    setSessions([session("s1"), session("s2", "OfferLab")], "s2");
    expect(sessionsSnapshot().map((s) => s.sessionId)).toEqual(["s1", "s2"]);
    expect(activeSessionId()).toBe("s2");

    unsubscribe();
    setSessions([]);
    expect(seen).toEqual(["s1", "s2"]);
  });

  it("keeps the prompt each tab was handed, so the pane can go on showing it", () => {
    setSessions([session("s1"), session("s2")]);
    expect(sessionsSnapshot()[0].prompt).toBe("fix the calendar header");
  });

  it("says what a tab was given, because a session that can send mail is not the same thing", () => {
    const withTools = { ...session("s1"), resources: ["Mach's tools"] };
    setSessions([withTools, session("s2")]);
    expect(sessionsSnapshot()[0].resources).toEqual(["Mach's tools"]);
    expect(sessionsSnapshot()[1].resources).toEqual([]);
  });

  it("steps through the tabs, wrapping at both ends", () => {
    setSessions([session("s1"), session("s2"), session("s3")], "s1");

    stepSession(1);
    expect(activeSessionId()).toBe("s2");
    stepSession(1);
    stepSession(1);
    // The end wraps round to the start, and the start back round to the end.
    expect(activeSessionId()).toBe("s1");
    stepSession(-1);
    expect(activeSessionId()).toBe("s3");
  });

  it("does nothing on a step with one tab, so the key can belong to the sidebar", () => {
    setSessions([session("s1")], "s1");
    stepSession(1);
    expect(activeSessionId()).toBe("s1");
  });

  it("hands the front to a neighbour when the front tab goes, and leaves the rest alone", () => {
    setSessions([session("s1"), session("s2"), session("s3")], "s2");

    // The list arriving without s2 is what closing it looks like from here.
    setSessions([session("s1"), session("s3")]);
    expect(activeSessionId()).toBe("s3");
    expect(sessionsSnapshot().map((s) => s.sessionId)).toEqual(["s1", "s3"]);
  });

  it("keeps the front tab in front when the list is refreshed around it", () => {
    setSessions([session("s1"), session("s2")], "s1");
    setSessions([session("s1"), session("s2"), session("s3")]);
    expect(activeSessionId()).toBe("s1");
  });

  it("ignores a request to select something that is not a tab", () => {
    setSessions([session("s1")], "s1");
    selectSession("s9");
    expect(activeSessionId()).toBe("s1");
  });

  it("has no front tab when there are none", () => {
    setSessions([]);
    expect(activeSessionId()).toBeNull();
    expect(sessionsSnapshot()).toEqual([]);
  });
});

describe("decodeChunk", () => {
  it("returns bytes rather than a string", () => {
    // Output crosses as base64 because a pty carries bytes: escape sequences,
    // and characters that a chunk boundary can land in the middle of. The
    // second case below is the first byte of a three-byte character — decoding
    // it as text here would corrupt it, and handing the emulator the byte lets
    // it wait for the rest.
    expect([...decodeChunk("G1swbQ==")]).toEqual([0x1b, 0x5b, 0x30, 0x6d]);
    expect([...decodeChunk("4g==")]).toEqual([0xe2]);
    expect([...decodeChunk("")]).toEqual([]);
  });
});
