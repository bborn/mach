// @vitest-environment jsdom

/**
 * The `From` row.
 *
 * There was no way to choose the account a new message went from. It was
 * decided once, at `c`, out of whatever the list happened to be filtered to —
 * and after that the only lever was the default in preferences, which is about
 * every message rather than this one. Reported as "I have no from control when
 * composing".
 *
 * Two rules decide when the row exists, and both are about there being a choice
 * to make rather than about taste:
 *
 *  * **One account is not a choice.** The row would be a control with a single
 *    item in it, on every composer, for ever.
 *  * **A reply has no choice either.** It answers a conversation one account
 *    holds — `reply_to_id`, the `References` chain built from it and the thread
 *    it is mirrored into are all that account's rows — and Gmail has no call
 *    that answers one account's thread as another. The dock withholds the
 *    handler for a reply, and `ipc::compose` refuses the move as well, so this
 *    is drawn nowhere it could promise something the store would then refuse.
 *
 * Rendered with `react-dom/server`, which is this file's neighbours' habit and
 * the reason it can assert on the markup a browser first lays hands on. The
 * popup itself is Base UI's and lives in a portal that a closed select does not
 * render; what is checked here is the trigger, which is the whole of the
 * keyboard surface.
 */

import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { KeymapProvider } from "@/hooks/useKeymap";
import type { Account } from "@/types";
import type { Draft, DraftKind } from "@/lib/compose";
import { Composer } from "./Composer";

const ACCOUNTS: Account[] = [
  { id: 1, email: "bruno@example.com", name: "Personal", colorIndex: 1, kind: "personal" },
  { id: 2, email: "bruno@northwind.example", name: "Northwind", colorIndex: 3, kind: "workspace" },
];

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

function render(
  over: Partial<Draft> = {},
  props: Partial<Parameters<typeof Composer>[0]> = {},
): HTMLElement {
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
        fromAccounts={ACCOUNTS}
        onChangeAccount={() => {}}
        {...props}
      />
    </KeymapProvider>,
  );
  return host;
}

/** The trigger, found the way a screen reader would: by the word beside it. */
function fromField(host: HTMLElement): HTMLElement | null {
  const label = [...host.querySelectorAll("span")].find(
    (span) => span.textContent?.trim() === "From" && span.id,
  );
  if (!label) return null;
  return host.querySelector<HTMLElement>(`[aria-labelledby~="${label.id}"]`);
}

describe("the composer's From row", () => {
  it("names the account the message is going from", () => {
    expect(fromField(render())?.textContent).toContain("bruno@example.com");
    expect(fromField(render({ accountId: 2 }))?.textContent).toContain(
      "bruno@northwind.example",
    );
  });

  it("is not drawn when there is only one account to choose", () => {
    expect(fromField(render({}, { fromAccounts: [ACCOUNTS[0]] }))).toBeNull();
  });

  it("is not drawn on a reply, which goes from the account holding the thread", () => {
    expect(fromField(render({}, { onChangeAccount: undefined }))).toBeNull();
  });

  /*
   * Everything is keyboard navigable. A `<button>` is a tab stop and a popup
   * Base UI opens on ⏎, ␣ and the arrow keys — which is the whole requirement.
   * A `<div>` with a click handler would look identical and satisfy none of it.
   */
  it("is reachable without a mouse", () => {
    const field = fromField(render());
    expect(field?.tagName).toBe("BUTTON");
    expect(field?.getAttribute("tabindex")).not.toBe("-1");
  });

  /*
   * Named by the word beside it *and* by its own contents, so it is announced
   * as "From bruno@example.com". An `aria-label` on the trigger would have
   * replaced the address rather than introduced it — the half that matters,
   * since the whole point of the control is which address.
   */
  it("is announced with the address, not just the word From", () => {
    const host = render();
    const field = fromField(host)!;
    const names = field
      .getAttribute("aria-labelledby")!
      .split(" ")
      .map((id) => host.querySelector(`#${id}`)?.textContent?.trim() ?? "")
      .join(" ");
    expect(names).toContain("From");
    expect(names).toContain("bruno@example.com");
  });

  /*
   * The row belongs to the block of addresses and above To, because that is
   * what it is — the other end of the same address. It read as an afterthought
   * anywhere else.
   */
  it("sits above To", () => {
    const host = render();
    const field = fromField(host)!;
    const to = host.querySelector('input[placeholder="name@example.com"]')!;
    expect(field.compareDocumentPosition(to) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  });

  it("goes read-only while the message is being sent", () => {
    expect(fromField(render({}, { busy: true }))?.hasAttribute("disabled")).toBe(true);
  });
});
