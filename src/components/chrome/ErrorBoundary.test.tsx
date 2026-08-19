// @vitest-environment jsdom

/**
 * The boundary has to catch, and it has to say what it caught.
 *
 * A boundary that renders nothing useful is the bug it exists to fix, wearing a
 * different hat — this pins that the message, the stack and a way out are all
 * on screen, and that a healthy tree is passed through untouched.
 *
 * React logs a caught error to `console.error` itself, on top of the one this
 * component writes. Both are silenced for the duration so a passing run is not
 * a wall of red that trains everybody to ignore it.
 */

import { createRoot } from "react-dom/client";
import { act } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { ErrorBoundary } from "./ErrorBoundary";

function Boom(): never {
  throw new Error("the calendar grid exploded");
}

let host: HTMLDivElement;

beforeEach(() => {
  vi.spyOn(console, "error").mockImplementation(() => {});
  host = document.createElement("div");
  document.body.appendChild(host);
});

afterEach(() => {
  vi.restoreAllMocks();
  host.remove();
});

function render(node: React.ReactNode): string {
  const root = createRoot(host);
  act(() => root.render(node));
  return host.innerHTML;
}

describe("ErrorBoundary", () => {
  it("lets a tree that works through without touching it", () => {
    const markup = render(
      <ErrorBoundary>
        <p>the inbox</p>
      </ErrorBoundary>,
    );
    expect(markup).toBe("<p>the inbox</p>");
  });

  it("catches a throw instead of leaving a blank page", () => {
    const markup = render(
      <ErrorBoundary>
        <Boom />
      </ErrorBoundary>,
    );
    expect(markup).toContain("data-mach-crashed");
    expect(markup).not.toBe("");
  });

  it("puts the actual error on the screen, not a friendly nothing", () => {
    const markup = render(
      <ErrorBoundary>
        <Boom />
      </ErrorBoundary>,
    );
    // The message is the whole point: "something went wrong" would leave the
    // reader exactly where the blank page did.
    expect(markup).toContain("the calendar grid exploded");
  });

  it("offers a way out and a way to report it", () => {
    const markup = render(
      <ErrorBoundary>
        <Boom />
      </ErrorBoundary>,
    );
    expect(markup).toContain("Reload");
    expect(markup).toContain("Copy the error");
  });

  it("says the mail is safe, because that is the first thing anyone wonders", () => {
    const markup = render(
      <ErrorBoundary>
        <Boom />
      </ErrorBoundary>,
    );
    expect(markup).toContain("Nothing has been lost");
  });
});
