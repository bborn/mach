import { describe, expect, it } from "vitest";
import { openableLatestId } from "./opening-message";

const sent = (id: number) => ({ id, isDraft: false });
const draft = (id: number) => ({ id, isDraft: true });

describe("openableLatestId", () => {
  it("opens the newest message", () => {
    expect(openableLatestId([sent(1), sent(2), sent(3)])).toBe(3);
  });

  it("skips a draft sitting at the end of the thread", () => {
    expect(openableLatestId([sent(1), sent(2), draft(3)])).toBe(2);
  });

  it("skips however many drafts are stacked there", () => {
    expect(openableLatestId([sent(1), draft(2), draft(3)])).toBe(1);
  });

  it("has nothing to open in a thread that is only a draft", () => {
    expect(openableLatestId([draft(1)])).toBeNull();
    expect(openableLatestId([])).toBeNull();
  });

  it("treats a message with no draft flag as a real one", () => {
    expect(openableLatestId([{ id: 7 }])).toBe(7);
  });
});
