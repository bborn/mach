import { readFileSync } from "node:fs";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { ScrollArea } from "./scroll-area";

describe("ScrollArea", () => {
  it("keeps the classic track on when the reading pane asks", () => {
    const html = renderToStaticMarkup(<ScrollArea lockGutter>body</ScrollArea>);
    expect(html).toContain("overflow-y:scroll");
    expect(html).toContain("scrollbar-gutter:stable");
  });

  it("leaves short panes on auto, so the list is not a dead strip", () => {
    const html = renderToStaticMarkup(<ScrollArea>body</ScrollArea>);
    expect(html).not.toContain("overflow-y:scroll");
    expect(html).toContain("overflow-y-auto");
  });

  it("the reading pane is the one that locks the gutter", () => {
    const src = readFileSync(new URL("../mail/ReadingPane.tsx", import.meta.url), "utf8");
    expect(src).toContain("lockGutter");
    expect(src).toContain("var(--scrollbar-size)");
  });
});
