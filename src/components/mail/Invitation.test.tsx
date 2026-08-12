// @vitest-environment jsdom

/**
 * Answering an invitation without leaving the app.
 *
 * Mounted for real, against a data source that records the command it was
 * handed. The claim being pinned is not "a card renders" — it is that pressing
 * Yes puts `{kind: "rsvp", eventId, response}` on the wire with the *right*
 * event on it, that the chord does the same thing as the button, and that an
 * invitation whose event is not in the store offers nothing at all rather than
 * a button that would dispatch nothing.
 *
 * Never against a real mailbox, and there is nothing here that could be: the
 * source is a stub, and what is asserted is the request that would have gone
 * out. An RSVP cannot be taken back, so a test that sent one would be telling
 * an organiser something on the owner's behalf.
 */

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { KeymapProvider } from "@/hooks/useKeymap";
import { MachProvider } from "@/hooks/useMach";
import {
  fixtureSource,
  setDataSource,
  type Command,
  type CommandResult,
  type MachDataSource,
} from "@/lib/data";
import type { Invitation as InvitationData, Message } from "@/types";
import { REQUEST } from "@/lib/invitation";
import { Invitation } from "./Invitation";
import { ThreadMessage } from "./ThreadMessage";

declare global {
  var IS_REACT_ACT_ENVIRONMENT: boolean | undefined;
}

const EVENT_ID = 77;

function invitation(over: Partial<InvitationData> = {}): InvitationData {
  return {
    uid: "6r2h1c9k@google.com",
    method: REQUEST,
    eventId: EVENT_ID,
    title: "Quarterly review",
    start: Date.parse("2026-08-12T15:00:00"),
    end: Date.parse("2026-08-12T16:00:00"),
    allDay: false,
    location: "Room 4",
    recurring: false,
    ...over,
  };
}

function message(over: Partial<Message> = {}): Message {
  return {
    id: 512,
    threadId: 41,
    accountId: 3,
    from: { name: "Alex Rivera", email: "alex@example.com" },
    to: [{ name: "Bruno", email: "bruno@example.com" }],
    cc: [],
    timestamp: Date.parse("2026-08-07T09:00:00"),
    bodyText: "Yes / No / Maybe",
    snippet: "Quarterly review",
    attachments: [],
    isDraft: false,
    ...over,
  };
}

/** A source that answers every command and keeps what it was asked. */
function recordingSource(ok = true) {
  const sent: Command[] = [];
  const source: MachDataSource = {
    ...fixtureSource,
    async listAccounts() {
      return [];
    },
    async listLabels() {
      return [];
    },
    async listCalendars() {
      return [];
    },
    async listEvents() {
      return [];
    },
    async listThreads() {
      return { threads: [], nextCursor: null };
    },
    async getThread() {
      return null;
    },
    async execute(command): Promise<CommandResult> {
      sent.push(command);
      return ok
        ? { ok: true, message: "Accepted", applied: [EVENT_ID], failed: [] }
        : {
            ok: false,
            message: "Could not send the RSVP",
            applied: [],
            failed: [
              {
                ids: [EVENT_ID],
                kind: "forbidden",
                message: "403 forbidden",
                retriable: false,
                rolledBack: true,
              },
            ],
          };
    },
  };
  return { source, sent };
}

let container: HTMLDivElement;
let root: Root;

beforeEach(() => {
  globalThis.IS_REACT_ACT_ENVIRONMENT = true;
  if (!window.matchMedia) {
    window.matchMedia = ((query: string) => ({
      matches: false,
      media: query,
      onchange: null,
      addEventListener() {},
      removeEventListener() {},
      addListener() {},
      removeListener() {},
      dispatchEvent: () => false,
    })) as unknown as typeof window.matchMedia;
  }
  container = document.createElement("div");
  document.body.append(container);
  root = createRoot(container);
});

afterEach(() => {
  act(() => root.unmount());
  container.remove();
  setDataSource(fixtureSource);
});

async function flush() {
  await act(async () => {
    await Promise.resolve();
    await Promise.resolve();
    await Promise.resolve();
  });
}

async function mount(node: React.ReactNode) {
  await act(async () => {
    root.render(
      <KeymapProvider>
        <MachProvider>{node}</MachProvider>
      </KeymapProvider>,
    );
  });
  await flush();
}

/** The three answer buttons, by their visible words. */
function answers(): HTMLButtonElement[] {
  return [...container.querySelectorAll("button")].filter((button) =>
    ["Yes", "Maybe", "No"].includes(button.textContent?.trim() ?? ""),
  ) as HTMLButtonElement[];
}

function press(...tokens: string[]) {
  for (const key of tokens) {
    window.dispatchEvent(new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true }));
  }
}

describe("the card", () => {
  it("is not drawn for ordinary mail", async () => {
    await mount(
      <ThreadMessage message={message()} live={false} expanded onToggle={() => {}} />,
    );
    expect(container.querySelector("[data-invitation]")).toBeNull();
    expect(answers()).toHaveLength(0);
  });

  it("is drawn for an invitation, with the meeting on it", async () => {
    await mount(
      <ThreadMessage
        message={message({ invitation: invitation() })}
        live={false}
        expanded
        onToggle={() => {}}
      />,
    );
    const card = container.querySelector("[data-invitation]");
    expect(card).not.toBeNull();
    expect(card?.textContent).toContain("Quarterly review");
    expect(card?.textContent).toContain("Room 4");
    expect(answers().map((b) => b.textContent?.trim())).toEqual(["Yes", "Maybe", "No"]);
  });

  /*
   * Google's own Yes / No / Maybe are links to google.com. With a native
   * control on screen, two sets of the same three words — one of which leaves
   * the app — is the confusion this feature exists to end, so the message body
   * starts folded. Folded, not rewritten: the disclosure is right there and
   * what comes back is the message exactly as it was sent.
   */
  it("folds Google's copy of the invitation away, behind a control that brings it back", async () => {
    await mount(
      <ThreadMessage
        message={message({ invitation: invitation() })}
        live={false}
        expanded
        onToggle={() => {}}
      />,
    );
    const disclosure = [...container.querySelectorAll("button")].find((b) =>
      b.textContent?.includes("Google's invitation"),
    );
    expect(disclosure, "the folded body has a control").toBeDefined();
    expect(disclosure?.getAttribute("aria-expanded")).toBe("false");

    await act(async () => disclosure?.click());
    await flush();
    expect(
      [...container.querySelectorAll("button")].some((b) =>
        b.textContent?.includes("Hide Google's invitation"),
      ),
    ).toBe(true);
  });

  it("leaves Google's copy alone when there is nothing native to press", async () => {
    await mount(
      <ThreadMessage
        message={message({ invitation: invitation({ eventId: undefined, title: undefined }) })}
        live={false}
        expanded
        onToggle={() => {}}
      />,
    );
    expect(
      [...container.querySelectorAll("button")].some((b) =>
        b.textContent?.includes("Google's invitation"),
      ),
      "the body is the reader's only remaining affordance and must stay open",
    ).toBe(false);
  });
});

describe("the event is not in the store", () => {
  it("says so, and offers nothing to press", async () => {
    await mount(
      <Invitation messageId={512} invitation={invitation({ eventId: undefined, title: undefined })} />,
    );
    expect(container.textContent).toContain("Not on this calendar");
    expect(answers()).toHaveLength(0);
  });
});

describe("answering", () => {
  it("dispatches Rsvp with the event and the response", async () => {
    const { source, sent } = recordingSource();
    setDataSource(source);
    await mount(<Invitation messageId={512} invitation={invitation()} />);

    const maybe = answers().find((b) => b.textContent?.trim() === "Maybe");
    await act(async () => maybe?.click());
    await flush();

    expect(sent).toEqual([{ kind: "rsvp", eventId: EVENT_ID, response: "tentative" }]);
  });

  it("answers from the keyboard, on the same command", async () => {
    const { source, sent } = recordingSource();
    setDataSource(source);
    await mount(<Invitation messageId={512} invitation={invitation()} />);

    // `i` opens the chord; `y` completes it.
    await act(async () => press("i", "y"));
    await flush();
    expect(sent).toEqual([{ kind: "rsvp", eventId: EVENT_ID, response: "accepted" }]);

    // And the other two, so the chord is a vocabulary rather than one key.
    sent.length = 0;
    await act(async () => press("i", "n"));
    await flush();
    await act(async () => press("i", "m"));
    await flush();
    expect(sent.map((c) => ("response" in c ? c.response : null))).toEqual([
      "declined",
      "tentative",
    ]);
  });

  it("does not answer from the keyboard when there is no event to answer against", async () => {
    const { source, sent } = recordingSource();
    setDataSource(source);
    await mount(<Invitation messageId={512} invitation={invitation({ eventId: undefined })} />);

    await act(async () => press("i", "y"));
    await flush();
    expect(sent).toEqual([]);
  });

  it("shows the answer immediately, and marks it as the current one", async () => {
    const { source } = recordingSource();
    setDataSource(source);
    await mount(<Invitation messageId={512} invitation={invitation()} />);

    const yes = answers().find((b) => b.textContent?.trim() === "Yes");
    await act(async () => yes?.click());
    await flush();

    const pressed = answers().filter((b) => b.getAttribute("aria-pressed") === "true");
    expect(pressed.map((b) => b.textContent?.trim())).toEqual(["Yes"]);
    expect(container.textContent).toContain("you said Yes");
  });

  it("puts the button back and says why when Google refuses", async () => {
    const { source } = recordingSource(false);
    setDataSource(source);
    await mount(
      <Invitation messageId={512} invitation={invitation({ response: "accepted" })} />,
    );

    const no = answers().find((b) => b.textContent?.trim() === "No");
    await act(async () => no?.click());
    await flush();

    // Google's own reason, not a shrug: `describeResult` is what the status bar
    // says, and the card says it too, in place, where the button was pressed.
    expect(container.textContent).toContain("403 forbidden");
    const pressed = answers().filter((b) => b.getAttribute("aria-pressed") === "true");
    expect(pressed.map((b) => b.textContent?.trim()), "the prior answer stands").toEqual(["Yes"]);
  });
});

describe("an answer already given", () => {
  it("is shown rather than three blank buttons", async () => {
    await mount(
      <Invitation messageId={512} invitation={invitation({ response: "tentative" })} />,
    );
    const pressed = answers().filter((b) => b.getAttribute("aria-pressed") === "true");
    expect(pressed.map((b) => b.textContent?.trim())).toEqual(["Maybe"]);
    expect(container.textContent).toContain("you said Maybe");
  });

  it("is nothing to show when the invitation is unanswered", async () => {
    await mount(
      <Invitation messageId={512} invitation={invitation({ response: "needsAction" })} />,
    );
    expect(answers().filter((b) => b.getAttribute("aria-pressed") === "true")).toHaveLength(0);
    expect(container.textContent).not.toContain("you said");
  });
});
