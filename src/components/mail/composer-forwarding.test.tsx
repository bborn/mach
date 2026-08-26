// @vitest-environment jsdom

/**
 * What a forward says it is carrying.
 *
 * Asked as "does forwarding an email also forward the contents of the email?",
 * against a composer showing an empty body under a `Fwd:` subject. It does —
 * `draft::forward_text` and `forward_html` reproduce the original whole, with
 * its own headers, at build time — but nothing on screen said so, and the only
 * way to find out was to send it and ask the recipient.
 *
 * The strip is a label, not the text. The block that leaves is built in Rust
 * from the stored message at send; drawing it here would be a second copy that
 * could disagree with what actually goes.
 */

import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { KeymapProvider } from "@/hooks/useKeymap";
import type { Draft, DraftKind } from "@/lib/compose";
import { Composer } from "./Composer";

function draft(over: Partial<Draft> = {}): Draft {
  return {
    id: "d1",
    accountId: 1,
    kind: "new" as DraftKind,
    to: [],
    cc: [],
    bcc: [],
    subject: "",
    body: "",
    bodyFormat: "html",
    updatedAt: 0,
    ...over,
  };
}

function text(over: Partial<Draft> = {}, props: Record<string, unknown> = {}): string {
  const host = document.createElement("div");
  host.innerHTML = renderToStaticMarkup(
    <KeymapProvider>
      <Composer
        draft={draft(over)}
        html=""
        bodyHeight={340}
        onChange={() => {}}
        onBodyChange={() => {}}
        onSend={() => {}}
        onClose={() => {}}
        onDiscard={() => {}}
        onAttach={() => {}}
        onRemoveAttachment={() => {}}
        {...props}
      />
    </KeymapProvider>,
  );
  return host.textContent ?? "";
}

const FORWARDING = { subject: "Are Zev's passport details in order?", from: "EF Educational Tours" };

describe("the forwarding strip", () => {
  it("names the message the recipient is about to be sent", () => {
    const out = text({ kind: "forward" as DraftKind, subject: "Fwd: Are Zev's…" }, {
      forwarding: FORWARDING,
    });
    expect(out).toContain("Forwarding");
    expect(out).toContain("Are Zev's passport details in order?");
    expect(out).toContain("EF Educational Tours");
  });

  it("says nothing when there is no parent to name", () => {
    expect(text({ kind: "forward" as DraftKind })).not.toContain("Forwarding");
  });

  /*
   * A reply quotes too, and invisibly, and that is left alone: every mail
   * client on earth quotes on reply and nobody is surprised by it. The forward
   * is the one that shows an empty body while sending somebody else's whole
   * message.
   */
  it("is not drawn on a reply or a new message", () => {
    expect(text({ kind: "reply" as DraftKind, subject: "Re: x" })).not.toContain("Forwarding");
    expect(text({ kind: "new" as DraftKind })).not.toContain("Forwarding");
  });

  it("survives a message from somebody with no display name", () => {
    const out = text({ kind: "forward" as DraftKind }, {
      forwarding: { subject: "Invoice", from: "" },
    });
    expect(out).toContain("Forwarding");
    expect(out).toContain("Invoice");
  });
});
