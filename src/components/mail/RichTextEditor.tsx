import {
  useCallback,
  useEffect,
  useImperativeHandle,
  useRef,
  useState,
  type Ref,
  type RefObject,
} from "react";
import Squire from "squire-rte";
import { Bold, Italic, Underline, List, ListOrdered, Quote, Link2 } from "lucide-react";
import { useKeyBindings } from "@/hooks/useKeymap";
import { cleanFragment, isBlankHtml } from "@/lib/email-html";
import { cn } from "@/lib/utils";
import { caretOffsetIn, caretRangeIn } from "./caret-offset";
import { COMPOSER_BODY, COMPOSER_COLUMN, COMPOSER_FIXED_ROW } from "./composer-layout";
import { COMPOSER_KEYS } from "@/lib/compose";

/**
 * The composer's editor.
 *
 * # Why Squire
 *
 * It was written for a mail client, by people shipping one, and it edits HTML
 * directly — which is the format that goes on the wire, so there is no document
 * model to convert and nothing to lose in the conversion. ProseMirror and TipTap
 * are better *document* editors and would need a serializer to email-safe HTML
 * that would then be the thing to get right. It is also about sixty kilobytes
 * with its sanitizer bundled and has no dependencies, which matters in a project
 * with ten runtime ones that writes its own UI primitives.
 *
 * # React does not own this DOM
 *
 * Squire mutates the contenteditable directly. So the element it edits is
 * rendered once, with no React children, and never re-rendered — every prop that
 * would change it is applied through the editor's own API instead. `docKey` is
 * the one exception: when it changes, a *different draft* is being edited and
 * the whole document is replaced.
 *
 * # Undo
 *
 * Squire keeps its own undo stack and binds ⌘Z to it. The app's undo is a
 * different thing entirely — it takes back an archive or a label change — and
 * `keymap` already declines to fire ordinary bindings while the target
 * `isContentEditable`, so the two do not have to negotiate. The one binding that
 * does reach in here is "undo send", which is registered `allowInInput` and only
 * lives while a message is in the outbox; inside those ten seconds ⌘Z recalls
 * the message, which is what the user means, and outside them it undoes typing.
 */

export interface RichTextEditorHandle {
  focus(): void;
  /** The current HTML, read straight out of the editor rather than from state. */
  html(): string;
  /**
   * Put markup in at the caret. Used for one thing: an image being placed in
   * the body rather than attached beside it.
   */
  insert(html: string): void;
  /**
   * Take an inline image back out — the other half of the same choice.
   *
   * Removes the node rather than replacing the document, so the caret and the
   * undo stack survive. The image is identified by its content id, which is
   * what the chip and the part both carry.
   */
  removeInline(contentId: string): void;
  /**
   * Where the caret is, as a character offset into the message.
   *
   * Read before the composer is moved between the dock and the window, which
   * unmounts this editor and builds another — see `caret-offset.ts` for why a
   * number is the only form of the answer that survives that.
   */
  caret(): number | null;
}

interface RichTextEditorProps {
  /**
   * Identity of the document being edited. A change replaces the contents; a
   * re-render with the same key leaves the editor alone, which is what keeps
   * the cursor where the user put it.
   */
  docKey: string;
  initialHtml: string;
  onChange: (html: string) => void;
  disabled?: boolean;
  placeholder?: string;
  /**
   * How tall the writing area stands. Past it the editor scrolls.
   *
   * A height rather than a maximum, because it is the thing the handle on the
   * composer's top edge moves: a box that grew with the text would answer a
   * drag with a size the next keystroke changed again.
   */
  height?: number;
  /**
   * Where to put the caret once the document is in, as a character offset.
   *
   * Only read when a document is loaded — on mount, and when `docKey` names a
   * different draft — so it is a starting position, not a controlled value.
   */
  initialCaret?: number | null;
  handle?: Ref<RichTextEditorHandle>;
  /** Whether this editor's keys are live. False for a composer in the background. */
  active?: boolean;
  /**
   * Somewhere for the surface around the composer to hold on to the writing
   * area — the popped-out overlay places focus there. Mirrored from the ref
   * this component keeps for Squire rather than replacing it: React owns this
   * element exactly once, at mount, and Squire owns everything inside it.
   */
  bodyRef?: RefObject<HTMLElement | null>;
  /**
   * `cid:` → `data:` URL, for the images that are part of this message.
   *
   * Applied to the editor's DOM directly rather than through `initialHtml`,
   * because the bytes arrive from SQLite after the document is already in and
   * being typed into. Replacing the document to show a picture would take the
   * caret and the undo stack with it.
   */
  inlineImages?: ReadonlyMap<string, string>;
}

export function RichTextEditor({
  docKey,
  initialHtml,
  onChange,
  disabled = false,
  placeholder = "Write your message",
  height = 340,
  initialCaret = null,
  handle,
  active = true,
  bodyRef,
  inlineImages,
}: RichTextEditorProps) {
  const root = useRef<HTMLDivElement>(null);
  const editor = useRef<Squire | null>(null);
  const latest = useRef(onChange);
  latest.current = onChange;
  // Read when a document is loaded, never depended on: a re-render because the
  // caret moved would be a re-render on every keystroke.
  const startAt = useRef(initialCaret);
  startAt.current = initialCaret;

  const [empty, setEmpty] = useState(() => isBlankHtml(initialHtml));
  /** Which formats the cursor is inside, so the toolbar reflects the text. */
  const [path, setPath] = useState("");
  const [linking, setLinking] = useState(false);
  const [linkUrl, setLinkUrl] = useState("");
  const linkField = useRef<HTMLInputElement>(null);

  // Built once per editor element. The dependency list is deliberately empty:
  // a rebuild would throw away the undo stack and the cursor.
  useEffect(() => {
    const node = root.current;
    if (!node) return;
    const instance = new Squire(node, {
      blockTag: "DIV",
      // Every paste, and every `setHTML`, goes through this. It is the same
      // allowlist Rust applies at send — see `lib/email-html`.
      sanitizeToDOMFragment: (html: string) => cleanFragment(html, node.ownerDocument),
      // A typed URL becomes a link, which is what every mail client does and
      // what the markdown editor did with its bare-URL rule.
      addLinks: true,
    });
    editor.current = instance;

    const report = () => {
      const html = instance.getHTML();
      setEmpty(isBlankHtml(html));
      latest.current(html);
    };
    instance.addEventListener("input", report);
    instance.addEventListener("pathChange", () => setPath(instance.getPath()));
    instance.addEventListener("select", () => setPath(instance.getPath()));

    return () => {
      editor.current = null;
      instance.destroy();
    };
  }, []);

  // A different draft: replace the document, and do it without telling anybody
  // it changed — `setHTML` is not an edit, and reporting it would mark a draft
  // dirty the moment it was opened.
  useEffect(() => {
    const instance = editor.current;
    if (!instance) return;
    instance.setHTML(initialHtml);
    setEmpty(isBlankHtml(initialHtml));
    setLinking(false);
    // Where the caret was before this editor existed — the composer moving
    // between the dock and the window is the case this is for. `setHTML` has
    // just rebuilt every node, so this has to happen after it.
    const node = root.current;
    const at = startAt.current;
    if (node && at != null) {
      const range = caretRangeIn(node, at);
      if (range) instance.setSelection(range);
    }
  }, [docKey]); // eslint-disable-line -- the body is not a prop, it is a document

  useEffect(() => {
    const node = root.current;
    if (node) node.contentEditable = disabled ? "false" : "true";
  }, [disabled]);

  /*
   * Draw the images that are parts of this message.
   *
   * An attribute on nodes Squire already owns, rather than a document replace:
   * `src` is not something the editor tracks, so setting it neither disturbs
   * the caret nor pushes anything onto the undo stack. Nothing is reported
   * upward either — the body has not changed, only what it is showing, and the
   * `cid:` form is what `changeBody` puts back on the draft in any case.
   *
   * Runs after `docKey` has replaced the document, whenever the bytes of one
   * more image arrive, and — the one that is not obvious — whenever the body
   * changes. That last dependency is what draws an image the *moment* it is
   * placed: `insert` reports the new HTML upward, the draft's body comes back
   * down as `initialHtml`, and this runs against a document that now has an
   * `<img>` in it. Without it the picture would appear only on the next thing
   * that happened to change the map.
   */
  useEffect(() => {
    const node = root.current;
    if (!node || !inlineImages || inlineImages.size === 0) return;
    for (const image of node.querySelectorAll("img")) {
      const src = image.getAttribute("src") ?? "";
      const contentId =
        image.getAttribute("data-mach-cid") ||
        (src.toLowerCase().startsWith("cid:") ? src.slice(4) : "");
      if (!contentId) continue;
      const url = inlineImages.get(contentId);
      if (!url || src === url) continue;
      image.setAttribute("data-mach-cid", contentId);
      image.setAttribute("src", url);
    }
  }, [inlineImages, docKey, initialHtml]);

  useImperativeHandle(
    handle,
    () => ({
      focus: () => editor.current?.focus(),
      html: () => editor.current?.getHTML() ?? "",
      insert: (html: string) => {
        const instance = editor.current;
        if (!instance) return;
        instance.focus();
        instance.insertHTML(html);
        setEmpty(isBlankHtml(instance.getHTML()));
        latest.current(instance.getHTML());
      },
      removeInline: (contentId: string) => {
        const node = root.current;
        const instance = editor.current;
        if (!node || !instance) return;
        let removed = false;
        for (const image of node.querySelectorAll("img")) {
          const src = image.getAttribute("src") ?? "";
          const owned =
            image.getAttribute("data-mach-cid") === contentId ||
            src.toLowerCase() === `cid:${contentId}`.toLowerCase();
          if (!owned) continue;
          image.remove();
          removed = true;
        }
        if (!removed) return;
        setEmpty(isBlankHtml(instance.getHTML()));
        latest.current(instance.getHTML());
      },
      caret: () => {
        const node = root.current;
        if (!node) return null;
        try {
          return caretOffsetIn(node, editor.current?.getSelection());
        } catch {
          // Squire asks the document for a selection it may not have. An
          // editor that was never typed into simply has no caret to keep.
          return null;
        }
      },
    }),
    [],
  );

  /** Run a Squire command and keep the toolbar in step with the result. */
  const apply = useCallback((run: (instance: Squire) => void) => {
    const instance = editor.current;
    if (!instance) return;
    run(instance);
    instance.focus();
    setPath(instance.getPath());
    const html = instance.getHTML();
    setEmpty(isBlankHtml(html));
    latest.current(html);
  }, []);

  const inside = (tag: string) => new RegExp(`(?:^|>)${tag}\\b`, "i").test(path);

  const toggle = useCallback(
    (tag: string, on: (i: Squire) => void, off: (i: Squire) => void) =>
      apply((instance) => (instance.hasFormat(tag) ? off(instance) : on(instance))),
    [apply],
  );

  const startLink = useCallback(() => {
    const instance = editor.current;
    if (!instance) return;
    // Pre-fill from the selection when it already looks like a URL: the common
    // case is "I pasted the address, now make it a link".
    const selected = instance.getSelectedText().trim();
    setLinkUrl(/^(https?:\/\/|www\.|mailto:)/i.test(selected) ? selected : "");
    setLinking(true);
    window.setTimeout(() => linkField.current?.focus(), 0);
  }, []);

  const commitLink = useCallback(() => {
    const url = linkUrl.trim();
    setLinking(false);
    if (!url) return;
    // A bare domain is what people type, and a link with no scheme resolves
    // against the recipient's mail client, which is nowhere.
    const href = /^[a-z][a-z0-9+.-]*:/i.test(url) ? url : `https://${url}`;
    apply((instance) => instance.makeLink(href));
  }, [apply, linkUrl]);

  useKeyBindings([
    {
      keys: COMPOSER_KEYS.link,
      group: "Composer",
      description: "Link",
      allowInInput: true,
      /*
       * The floor an overlay claims, which is where the rest of the composer's
       * keys sit. It used to be 210 — above the palette — so that a composer
       * took ⌘K away from search for as long as it was open. The key moved
       * instead; see `COMPOSER_KEYS.link`. Nothing outranks anything here now,
       * and 100 is what keeps this alive inside the popped-out overlay's claim.
       */
      priority: 100,
      when: () => active && !disabled,
      handler: () => (linking ? setLinking(false) : startLink()),
    },
  ]);

  return (
    // The one part of the composer that gives up space, so it carries no
    // `shrink-0` and its own column lets the squeeze reach the writing area
    // rather than stopping at this box. Unconstrained — the dock, where the
    // height is whatever the handle was dragged to — this lays out exactly as
    // the block it replaced.
    <div className={cn("mt-3", COMPOSER_COLUMN)}>
      {/*
        Not a tab stop, and not seven of them.
        ⇥ out of the address fields means "on to the message" — it always has,
        in every mail client — and this row sat in the way of it: bold, italic,
        underline, two lists, quote, link, and only then the body. The buttons
        are held out of the sequence with `tabIndex={-1}`, so ⇥ goes from the
        last address field straight into the writing area.

        Nothing becomes mouse-only by it. Every action here has a key, all of
        them Squire's own except the link, which `useKeyBindings` above
        registers as ⇧⌘K, and each button says which key in its tooltip. The
        buttons are what makes the formatting *visible* — the half a keyboard
        cannot do — and reading a toolbar is not something ⇥ is for.

        The alternative was the ARIA toolbar pattern: one tab stop, ← → between
        the buttons. It is the right shape for a toolbar whose actions have no
        keys. These all have keys, and it would still leave a stop between the
        To field and the body, which is the thing being fixed.
      */}
      <div className={cn(COMPOSER_FIXED_ROW, "flex items-center gap-0.5 pb-1.5")}>
        <Tool label="Bold" keys="⌘B" on={inside("B")} onClick={() => toggle("B", (i) => i.bold(), (i) => i.removeBold())}>
          <Bold className="size-3.5" />
        </Tool>
        <Tool label="Italic" keys="⌘I" on={inside("I")} onClick={() => toggle("I", (i) => i.italic(), (i) => i.removeItalic())}>
          <Italic className="size-3.5" />
        </Tool>
        <Tool
          label="Underline"
          keys="⌘U"
          on={inside("U")}
          onClick={() => toggle("U", (i) => i.underline(), (i) => i.removeUnderline())}
        >
          <Underline className="size-3.5" />
        </Tool>
        <span className="mx-1 h-3.5 w-px bg-border" aria-hidden />
        <Tool
          label="Bulleted list"
          keys="⌘⇧8"
          on={inside("UL")}
          onClick={() => apply((i) => (inside("UL") ? i.removeList() : i.makeUnorderedList()))}
        >
          <List className="size-3.5" />
        </Tool>
        <Tool
          label="Numbered list"
          keys="⌘⇧9"
          on={inside("OL")}
          onClick={() => apply((i) => (inside("OL") ? i.removeList() : i.makeOrderedList()))}
        >
          <ListOrdered className="size-3.5" />
        </Tool>
        {/* Two keys, because the button is a toggle and ⌘] only quotes. */}
        <Tool
          label="Quote"
          keys="⌘] / ⌘["
          on={inside("BLOCKQUOTE")}
          onClick={() =>
            apply((i) => (inside("BLOCKQUOTE") ? i.decreaseQuoteLevel() : i.increaseQuoteLevel()))
          }
        >
          <Quote className="size-3.5" />
        </Tool>
        {/* Spelled out rather than derived, like the six above it. The label
            and the binding are held together by a test — see `compose.test`. */}
        <Tool label="Link" keys="⌘⇧K" on={inside("A")} onClick={startLink}>
          <Link2 className="size-3.5" />
        </Tool>
      </div>

      {linking && (
        <div
          className={cn(
            COMPOSER_FIXED_ROW,
            "mb-1.5 flex items-center gap-2 border-b border-border pb-1.5",
          )}
        >
          <span className="text-micro uppercase tracking-wide text-faint-foreground">Link</span>
          <input
            ref={linkField}
            value={linkUrl}
            spellCheck={false}
            autoComplete="off"
            // A tab stop, so it needs a name — the "Link" beside it is a
            // sibling span, not a label this input is inside.
            aria-label="Link"
            placeholder="example.com"
            onChange={(event) => setLinkUrl(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter") {
                event.preventDefault();
                commitLink();
              } else if (event.key === "Escape") {
                event.preventDefault();
                setLinking(false);
                editor.current?.focus();
              }
            }}
            className={cn(
              "min-w-0 flex-1 bg-transparent text-body text-foreground",
              "placeholder:text-faint-foreground focus:outline-none",
            )}
          />
        </div>
      )}

      {/*
        The height lives here rather than on the contenteditable below it.
        A box with a pixel height is what the handle on the composer's top edge
        moves, and it is also the one box that has to be allowed to come back
        down when the panel around it is shorter than the window it was
        measured against — `min-h-0` is what permits that, and the writing area
        follows it at `h-full` and scrolls the text it can no longer show.
      */}
      <div className={cn("relative", COMPOSER_BODY)} style={{ height }}>
        {empty && (
          <span className="pointer-events-none absolute left-0 top-0 text-reading leading-[1.6] text-faint-foreground">
            {placeholder}
          </span>
        )}
        <div
          ref={(node) => {
            root.current = node;
            if (bodyRef) bodyRef.current = node;
          }}
          role="textbox"
          aria-multiline="true"
          aria-label="Message"
          spellCheck
          className={cn(
            "block h-full w-full overflow-y-auto bg-transparent",
            "text-reading leading-[1.6] text-foreground focus:outline-none",
            // The editor's own rendering, scoped here rather than added to the
            // global sheet: a `<ul>` inside a contenteditable needs a marker and
            // an indent to be recognisable as a list while it is being typed,
            // and none of this is what leaves — the outgoing HTML carries no
            // classes at all.
            "[&_ul]:list-disc [&_ol]:list-decimal [&_ul]:pl-5 [&_ol]:pl-5",
            "[&_blockquote]:border-l [&_blockquote]:border-border [&_blockquote]:pl-3",
            "[&_blockquote]:text-muted-foreground [&_a]:text-accent [&_a]:underline",
            "[&_pre]:whitespace-pre-wrap [&_code]:font-mono [&_code]:text-body",
            disabled && "opacity-60",
          )}
        />
      </div>
    </div>
  );
}

function Tool({
  label,
  keys,
  on,
  onClick,
  children,
}: {
  label: string;
  keys: string;
  on: boolean;
  onClick: () => void;
  children: React.ReactNode;
}) {
  return (
    <button
      type="button"
      // Not a toggle the mouse has to find: every one of these has a key, and
      // the title says which. The button exists so the formatting is *visible*,
      // which is the half a keyboard cannot do.
      title={`${label} (${keys})`}
      aria-label={label}
      aria-pressed={on}
      // Out of the tab sequence, so ⇥ from the last address field reaches the
      // message. See the note above the toolbar.
      tabIndex={-1}
      // The editor loses its selection when focus moves, and a toolbar button
      // that steals it would apply the format to nothing.
      onMouseDown={(event) => event.preventDefault()}
      onClick={onClick}
      className={cn(
        "inline-flex size-6 items-center justify-center rounded-[var(--radius)]",
        on ? "bg-hover text-foreground" : "text-faint-foreground hover:text-foreground",
      )}
    >
      {children}
    </button>
  );
}
