/**
 * The frontend half of attachments, tested as decisions rather than as pixels.
 *
 * Two things here are security-relevant and get most of the file:
 *
 * * `inlineImageUrl` is the last check before bytes from a stranger become a
 *   `data:` URL inside a message frame. Rust already restricts the type and the
 *   payload; this exists so that a change over there cannot quietly widen what
 *   this side writes into a document.
 * * `applyInlineImages` writes into sanitized HTML, so its pattern has to match
 *   exactly what the sanitizer emits and nothing that a sender could arrange.
 */

import { describe, expect, it, vi } from "vitest";
import {
  applyInlineImages,
  attachmentKind,
  attachmentLabel,
  contentIdsIn,
  disambiguateNames,
  displayFilename,
  extensionOf,
  fetchInlineImages,
  formatBytes,
  inlineImageUrl,
  isExecutable,
  openAttachment,
  saveAttachment,
  setAttachmentInvoker,
  type InvokeFn,
  type InlineImage,
} from "./attachments";

/** The transparent GIF `render::sanitize::PLACEHOLDER_PIXEL` substitutes in. */
const PLACEHOLDER =
  "data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7";

/** One `<img>` exactly as `promote_marker` writes it. */
function cidImage(contentId: string, extra = ""): string {
  return `<img${extra} data-mach-cid="${contentId}" src="${PLACEHOLDER}">`;
}

function image(over: Partial<InlineImage> = {}): InlineImage {
  return {
    contentId: "chart-001",
    mimeType: "image/png",
    base64: "iVBORw0KGgo=",
    ...over,
  };
}

/* -------------------------------------------------------------------------- */
/* Display                                                                     */
/* -------------------------------------------------------------------------- */

describe("formatBytes", () => {
  it("reads the way every other app on the machine reads", () => {
    expect(formatBytes(0)).toBe("0 B");
    expect(formatBytes(512)).toBe("512 B");
    expect(formatBytes(1024)).toBe("1 KB");
    expect(formatBytes(158204)).toBe("154 KB");
    expect(formatBytes(1024 * 1024)).toBe("1.0 MB");
    expect(formatBytes(12_582_912)).toBe("12.0 MB");
  });

  it("does not render a number nobody can act on", () => {
    expect(formatBytes(-1)).toBe("—");
    expect(formatBytes(Number.NaN)).toBe("—");
    expect(formatBytes(Number.POSITIVE_INFINITY)).toBe("—");
  });
});

describe("extensionOf", () => {
  it("takes the last one, lowercased", () => {
    expect(extensionOf("Q3-numbers.pdf")).toBe("pdf");
    expect(extensionOf("report.PDF")).toBe("pdf");
    expect(extensionOf("report.pdf.exe")).toBe("exe");
  });

  it("has no answer where there is no extension", () => {
    expect(extensionOf("notes")).toBeNull();
    expect(extensionOf(".bashrc")).toBeNull();
    expect(extensionOf("trailing.")).toBeNull();
    expect(extensionOf("")).toBeNull();
  });
});

describe("attachmentKind", () => {
  it("prefers the declared type", () => {
    expect(attachmentKind("image/png", "chart.png")).toBe("image");
    expect(attachmentKind("audio/mpeg", "clip.mp3")).toBe("audio");
    expect(attachmentKind("video/mp4", "clip.mp4")).toBe("video");
    expect(attachmentKind("application/pdf", "Q3.pdf")).toBe("document");
  });

  it("falls back to the extension, because senders declare octet-stream for everything", () => {
    expect(attachmentKind("application/octet-stream", "q3.csv")).toBe("spreadsheet");
    expect(attachmentKind("application/octet-stream", "logs.zip")).toBe("archive");
    expect(attachmentKind("application/octet-stream", "config.json")).toBe("code");
    expect(attachmentKind("application/octet-stream", "contract.docx")).toBe("document");
    expect(attachmentKind("application/octet-stream", "mystery")).toBe("file");
  });

  it("marks a program as a program before anything is clicked", () => {
    expect(attachmentKind("application/octet-stream", "setup.exe")).toBe("executable");
    expect(attachmentKind("image/png", "photo.jpg.scr")).toBe("executable");
    expect(attachmentKind("application/x-mach-binary", "notes")).toBe("executable");
  });
});

describe("isExecutable", () => {
  it("is a hint that agrees with the Rust refusal on the obvious cases", () => {
    for (const name of ["a.exe", "a.app", "a.command", "a.dmg", "a.jar", "a.sh", "a.webloc"]) {
      expect(isExecutable(name, "application/octet-stream")).toBe(true);
    }
    for (const name of ["a.pdf", "a.png", "a.csv", "a.zip", "attachment"]) {
      expect(isExecutable(name, "application/pdf")).toBe(false);
    }
  });
});

describe("displayFilename", () => {
  it("leaves a real name alone", () => {
    expect(displayFilename("Q3.pdf")).toBe("Q3.pdf");
    expect(displayFilename("契約書.pdf")).toBe("契約書.pdf");
  });

  // Every one of these is a shape that turns up in a real mailbox: the
  // `text/rfc822-headers` part of a bounce, Gmail's AMP alternative body, the
  // JSON behind an emoji reaction. Before this they rendered as a chip with a
  // size and no name at all.
  it("names the parts that arrived without one", () => {
    expect(displayFilename("")).toBe("attachment");
    expect(displayFilename("   ")).toBe("attachment");
  });
});

describe("disambiguateNames", () => {
  // Straight out of the owner's mailbox: one message with three copies of
  // `Plowing 2025.pdf`, and a pile of invitations carrying two `invite.ics`.
  it("tells identical names apart without touching the first one", () => {
    expect(
      disambiguateNames(["Plowing 2025.pdf", "Plowing 2025.pdf", "Plowing 2025.pdf"]),
    ).toEqual(["Plowing 2025.pdf", "Plowing 2025 (2).pdf", "Plowing 2025 (3).pdf"]);
  });

  it("matches case-insensitively, the way a filesystem would", () => {
    expect(disambiguateNames(["Invite.ics", "invite.ics"])).toEqual([
      "Invite.ics",
      "invite (2).ics",
    ]);
  });

  it("appends to a name with no extension rather than inventing one", () => {
    expect(disambiguateNames(["attachment", "attachment"])).toEqual([
      "attachment",
      "attachment (2)",
    ]);
  });

  it("leaves distinct names exactly as they are", () => {
    const names = ["a.pdf", "b.pdf", "c.png"];
    expect(disambiguateNames(names)).toEqual(names);
  });
});

describe("attachmentLabel", () => {
  it("puts everything the eye gets into words", () => {
    expect(attachmentLabel("Q3.pdf", "application/pdf", 158204)).toBe("Q3.pdf, 154 KB");
  });

  it("reads out the fallback name for a part that has none", () => {
    expect(attachmentLabel("", "text/rfc822-headers", 3349)).toBe("attachment, 3 KB");
  });

  it("says so when the thing cannot be opened", () => {
    expect(attachmentLabel("setup.exe", "application/octet-stream", 2048)).toBe(
      "setup.exe, 2 KB, a program Mach will not open",
    );
  });
});

/* -------------------------------------------------------------------------- */
/* Inline images — finding the references                                      */
/* -------------------------------------------------------------------------- */

describe("contentIdsIn", () => {
  it("finds what the sanitizer emitted, once each", () => {
    const html = `<p>${cidImage("chart-001")}${cidImage("logo@ex.com")}${cidImage("chart-001")}</p>`;
    expect(contentIdsIn(html)).toEqual(["chart-001", "logo@ex.com"]);
  });

  it("finds nothing in a body with no inline images", () => {
    expect(contentIdsIn("<p>hello</p>")).toEqual([]);
    expect(contentIdsIn("")).toEqual([]);
  });

  it("will not accept a Content-ID outside the shape Rust allows", () => {
    // Quotes, spaces and angle brackets are how an attacker would try to walk
    // the pattern past the end of the attribute. The sanitizer already rejects
    // them; this side does not depend on that.
    expect(contentIdsIn(`<img data-mach-cid="a b" src="${PLACEHOLDER}">`)).toEqual([]);
    expect(contentIdsIn(`<img data-mach-cid="../../etc" src="${PLACEHOLDER}">`)).toEqual([]);
    expect(contentIdsIn(`<img data-mach-cid="" src="${PLACEHOLDER}">`)).toEqual([]);
    expect(contentIdsIn(`<img data-mach-cid="${"a".repeat(513)}" src="x">`)).toEqual([]);
  });

  it("ignores an attribute that is not adjacent to the src it replaced", () => {
    expect(contentIdsIn(`<img data-mach-cid="x" alt="y" src="${PLACEHOLDER}">`)).toEqual([]);
  });
});

/* -------------------------------------------------------------------------- */
/* Inline images — the data URL                                                */
/* -------------------------------------------------------------------------- */

describe("inlineImageUrl", () => {
  it("builds a data URL for each raster type", () => {
    for (const mimeType of [
      "image/png",
      "image/jpeg",
      "image/gif",
      "image/webp",
      "image/bmp",
      "image/x-icon",
    ]) {
      expect(inlineImageUrl(image({ mimeType }))).toBe(`data:${mimeType};base64,iVBORw0KGgo=`);
    }
  });

  it("refuses SVG, which is a document and not a picture", () => {
    expect(inlineImageUrl(image({ mimeType: "image/svg+xml" }))).toBeNull();
  });

  it("refuses anything that is not an image at all", () => {
    expect(inlineImageUrl(image({ mimeType: "text/html" }))).toBeNull();
    expect(inlineImageUrl(image({ mimeType: "application/pdf" }))).toBeNull();
    expect(inlineImageUrl(image({ mimeType: "" }))).toBeNull();
  });

  it("refuses a payload that is not strict base64", () => {
    // Anything outside the base64 alphabet could close the attribute the URL is
    // written into, so it never gets that far.
    expect(inlineImageUrl(image({ base64: 'AAA" onerror=alert(1) x="' }))).toBeNull();
    expect(inlineImageUrl(image({ base64: "AAA<script>" }))).toBeNull();
    expect(inlineImageUrl(image({ base64: "AA A" }))).toBeNull();
    expect(inlineImageUrl(image({ base64: "" }))).toBeNull();
  });
});

/* -------------------------------------------------------------------------- */
/* Inline images — the substitution                                            */
/* -------------------------------------------------------------------------- */

describe("applyInlineImages", () => {
  const url = "data:image/png;base64,iVBORw0KGgo=";

  it("replaces the placeholder with the resolved image", () => {
    const html = `<p>Chart: ${cidImage("chart-001")}</p>`;
    const out = applyInlineImages(html, new Map([["chart-001", url]]));
    expect(out).toBe(`<p>Chart: <img data-mach-cid="chart-001" src="${url}"></p>`);
    expect(out).not.toContain(PLACEHOLDER);
  });

  it("keeps other attributes on the image", () => {
    const html = cidImage("chart-001", ' width="600" alt="Q3"');
    const out = applyInlineImages(html, new Map([["chart-001", url]]));
    expect(out).toContain('width="600"');
    expect(out).toContain('alt="Q3"');
    expect(out).toContain(`src="${url}"`);
  });

  it("leaves an unresolved reference on its placeholder", () => {
    const html = `${cidImage("chart-001")}${cidImage("missing-002")}`;
    const out = applyInlineImages(html, new Map([["chart-001", url]]));
    expect(out).toContain(`data-mach-cid="chart-001" src="${url}"`);
    expect(out).toContain(`data-mach-cid="missing-002" src="${PLACEHOLDER}"`);
  });

  it("replaces every occurrence of the same reference", () => {
    const html = `${cidImage("logo")}<p>x</p>${cidImage("logo")}`;
    const out = applyInlineImages(html, new Map([["logo", url]]));
    expect(out.split(url).length - 1).toBe(2);
  });

  it("does nothing when there is nothing resolved", () => {
    const html = cidImage("chart-001");
    expect(applyInlineImages(html, new Map())).toBe(html);
    expect(applyInlineImages("", new Map([["a", url]]))).toBe("");
  });

  it("cannot be aimed at anything but a reference Mach itself emitted", () => {
    // A sender cannot get `data-mach-cid` onto a real element: the sanitizer
    // drops every incoming `data-mach*` attribute. The most they can do is put
    // the literal text in a text node — where a match rewrites text into other
    // text and changes nothing that renders.
    const html = `<p>data-mach-cid="chart-001" src="whatever"</p>${cidImage("chart-001")}`;
    const out = applyInlineImages(html, new Map([["chart-001", url]]));
    expect(out).not.toContain("onerror");
    expect(out).not.toContain("javascript:");
    // The real image was still resolved.
    expect(out).toContain(`<img data-mach-cid="chart-001" src="${url}">`);
  });

  it("cannot be made to write a URL that breaks out of the attribute", () => {
    // The only way into the replacement is through `inlineImageUrl`, and every
    // hostile payload it is handed comes back null — so the map is empty and
    // the body is untouched.
    const hostile = inlineImageUrl(image({ base64: '"><script>alert(1)</script>' }));
    expect(hostile).toBeNull();
  });
});

/* -------------------------------------------------------------------------- */
/* Fetching                                                                    */
/* -------------------------------------------------------------------------- */

describe("fetchInlineImages", () => {
  it("resolves each reference independently", async () => {
    const resolved = await fetchInlineImages(1, ["a", "b"], async (_id, contentId) =>
      image({ contentId }),
    );
    expect([...resolved.keys()]).toEqual(["a", "b"]);
    expect(resolved.get("a")).toBe("data:image/png;base64,iVBORw0KGgo=");
  });

  it("keeps the ones that worked when one part is missing", async () => {
    const resolved = await fetchInlineImages(1, ["good", "gone"], async (_id, contentId) => {
      if (contentId === "gone") throw new Error("no part with that Content-ID");
      return image({ contentId });
    });
    expect([...resolved.keys()]).toEqual(["good"]);
  });

  it("drops a part that came back as something other than a raster image", async () => {
    const resolved = await fetchInlineImages(1, ["svg"], async (_id, contentId) =>
      image({ contentId, mimeType: "image/svg+xml" }),
    );
    expect(resolved.size).toBe(0);
  });

  it("asks for nothing when there are no references", async () => {
    const fetcher = vi.fn();
    const resolved = await fetchInlineImages(1, [], fetcher);
    expect(fetcher).not.toHaveBeenCalled();
    expect(resolved.size).toBe(0);
  });
});

/* -------------------------------------------------------------------------- */
/* The commands                                                                */
/* -------------------------------------------------------------------------- */

describe("the IPC surface", () => {
  it("names the commands and arguments the Rust side registered", async () => {
    const calls: Array<{ command: string; args?: Record<string, unknown> }> = [];
    const invoker: InvokeFn = async <T,>(command: string, args?: Record<string, unknown>) => {
      calls.push({ command, args });
      return (command === "attachment_save"
        ? { path: "/Users/x/Downloads/Q3.pdf", filename: "Q3.pdf" }
        : {
            attachmentId: 7,
            filename: "Q3.pdf",
            mimeType: "application/pdf",
            sizeBytes: 10,
            path: "/cache/Q3.pdf",
            fromCache: false,
          }) as T;
    };
    setAttachmentInvoker(invoker);
    try {
      await openAttachment(7);
      const saved = await saveAttachment(7);
      expect(saved.path).toBe("/Users/x/Downloads/Q3.pdf");
    } finally {
      setAttachmentInvoker(null);
    }

    expect(calls).toEqual([
      { command: "attachment_open", args: { attachmentId: 7 } },
      { command: "attachment_save", args: { attachmentId: 7 } },
    ]);
  });

  it("does not fetch anything by merely being imported", () => {
    // The whole module surface is functions. Nothing runs on import, so
    // rendering a thread cannot download a stranger's bytes as a side effect.
    const fetcher = vi.fn();
    setAttachmentInvoker(fetcher as unknown as InvokeFn);
    setAttachmentInvoker(null);
    expect(fetcher).not.toHaveBeenCalled();
  });
});
