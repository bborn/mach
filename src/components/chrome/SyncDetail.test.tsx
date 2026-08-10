/**
 * What a sync failure says, and what it offers to do about it.
 *
 * The report the owner got was "Sync failed", in eleven pixels of the bottom
 * rail, on a mailbox that had silently stopped updating — "but no way for me to
 * see how or why or fix it". Four things were missing and all four are asserted
 * here: which account, what Google actually said, when that account last
 * worked, and a recovery that matches the failure.
 *
 * Rendered with `react-dom/server`, no jsdom and nothing to click, for the
 * reason `Accounts.test.tsx` gives: what is worth pinning is the text and the
 * accessible names, and those survive being read as markup.
 */

import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { SyncFailure } from "@/lib/mailbox-state";
import { SyncDetail } from "./SyncIndicator";

/** Google's actual answer to a refresh after the account's password changed. */
const REVOKED =
  "google refused the stored credential: Google refused the stored credential: " +
  "invalid_grant (Token has been expired or revoked.)";

const NOW = new Date("2026-08-10T15:00:00Z").getTime();

function render(failures: SyncFailure[], lastPassFinishedAt: number | null = NOW - 60_000) {
  return renderToStaticMarkup(
    <SyncDetail
      failures={failures}
      lastPassFinishedAt={lastPassFinishedAt}
      onRetry={() => {}}
      onSignIn={() => {}}
      now={NOW}
    />,
  );
}

function labels(markup: string): string[] {
  return [...markup.matchAll(/aria-label="([^"]*)"/g)].map(([, value]) => value!);
}

const dead: SyncFailure = {
  email: "bruno@clickfunnels.example",
  message: REVOKED,
  needsReauthorization: true,
  lastSuccessAt: NOW - 3 * 86_400_000,
};

const throttled: SyncFailure = {
  email: "bruno@example.com",
  message: "google rate limited (429): User-rate limit exceeded",
  needsReauthorization: false,
  lastSuccessAt: NOW - 120_000,
};

describe("the sync detail", () => {
  it("names the account that failed", () => {
    expect(render([dead])).toContain("bruno@clickfunnels.example");
  });

  it("quotes Google rather than paraphrasing it", () => {
    const markup = render([dead]);
    // The whole point of showing this: `invalid_grant` plus Google's own
    // description is what distinguishes a password change from a withdrawn
    // grant from the seven-day expiry an unverified OAuth app puts on its
    // tokens. A summary would throw the answer away.
    expect(markup).toContain("invalid_grant");
    expect(markup).toContain("Token has been expired or revoked.");
  });

  it("says when that account last synced", () => {
    expect(render([dead])).toContain("Last synced");
    // An account that has never completed a pass in this run must not claim a
    // time it does not have.
    expect(render([{ ...dead, lastSuccessAt: null }])).toContain("Never synced");
  });

  it("offers the sign-in for a dead credential, not another sync", () => {
    const markup = render([dead]);
    expect(labels(markup)).toEqual([`Sign in again as ${dead.email}`]);
    expect(markup).not.toContain("Sync now");
  });

  it("offers a retry for a failure another pass could clear", () => {
    const markup = render([throttled]);
    expect(labels(markup)).toEqual([`Sync ${throttled.email} again`]);
    expect(markup).not.toContain("Sign in again");
  });

  it("gives each account its own recovery when they differ", () => {
    const markup = render([dead, throttled]);
    expect(labels(markup)).toEqual([
      `Sign in again as ${dead.email}`,
      `Sync ${throttled.email} again`,
    ]);
  });
});
