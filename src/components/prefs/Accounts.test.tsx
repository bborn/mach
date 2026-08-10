/**
 * The account row, and the one thing it used to be missing.
 *
 * "Needs authorization" was a bare `<span>`: the account was broken, the row
 * said so in red, and the only control beside it deleted the mail. The recovery
 * was an unlabelled inference — the "Add account" button below the list, which
 * reads as a way to connect a *new* account.
 *
 * So the assertions are about which controls exist on which row. Rendered with
 * `react-dom/server`, no jsdom and nothing to click, for the reason
 * `AccountRail.test.tsx` gives: what is worth pinning here is elements and
 * accessible names, and those survive being read as markup.
 */

import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { Account, ColorIndex } from "@/types";
import { Accounts } from "./PreferencesDialog";

function account(id: number, email: string, colorIndex: ColorIndex): Account {
  return { id, email, name: email, colorIndex, kind: "personal" };
}

const BROKEN = account(1, "bruno.bornsztein@gmail.com", 1);
const HEALTHY = account(2, "alex@lumen.example", 2);

function render(needsAuthorization: string[], reasons: Record<string, string> = {}) {
  return renderToStaticMarkup(
    <Accounts
      accounts={[BROKEN, HEALTHY]}
      needsAuthorization={needsAuthorization}
      reasons={reasons}
      confirming={null}
      onConfirm={() => {}}
      onRemoved={() => {}}
      onAdd={() => {}}
      onReauthorize={() => {}}
    />,
  );
}

/** Every `aria-label` in the markup, in document order. */
function labels(markup: string): string[] {
  return [...markup.matchAll(/aria-label="([^"]*)"/g)].map(([, value]) => value!);
}

describe("the account row", () => {
  it("offers a sign-in only to the account that needs one", () => {
    const markup = render([BROKEN.email]);

    expect(markup).toContain("Needs authorization");
    expect(markup).toContain("Sign in again");

    // One button, and it names the address it is for — the row is not the only
    // place this action can be reached from, and "Sign in again" on its own
    // would be three identical accessible names in a five-account list.
    const signIn = labels(markup).filter((label) => label.startsWith("Sign in again"));
    expect(signIn).toEqual([`Sign in again as ${BROKEN.email}`]);
  });

  it("leaves a healthy account with nothing to fix", () => {
    const markup = render([]);

    expect(markup).not.toContain("Needs authorization");
    expect(markup).not.toContain("Sign in again");
    // Which is also what a successful sign-in looks like from here: the address
    // leaves `needsReauthorization`, Rust emits the sync status, and the label
    // and its button go with it. No restart, no reload of the surface.
  });

  it("says what Google said, beside the button that acts on it", () => {
    // The status bar sends him here. The reason must not be left behind in the
    // panel that did the sending — this is where the recovery is, so this is
    // where the reason for it belongs.
    const markup = render([BROKEN.email], {
      [BROKEN.email]: "invalid_grant (Token has been expired or revoked.)",
    });
    expect(markup).toContain("invalid_grant");
    expect(markup).toContain("Token has been expired or revoked.");
  });

  it("says nothing extra when Google was never asked", () => {
    // A Keychain entry that is simply gone was never refused by anybody, so
    // there is no remote text and the row does not invent one.
    const markup = render([BROKEN.email]);
    expect(markup).toContain("Needs authorization");
    expect(markup).not.toContain("invalid_grant");
  });

  it("does not disturb Remove, which is still offered on every row", () => {
    const markup = render([BROKEN.email]);
    expect(labels(markup).filter((label) => label.startsWith("Remove"))).toEqual([
      `Remove ${BROKEN.email}`,
      `Remove ${HEALTHY.email}`,
    ]);
  });
});
