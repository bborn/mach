import { useCallback, useEffect, useImperativeHandle, useRef, useState, type Ref } from "react";
import Squire from "squire-rte";
import { Bold, Italic, Underline, List, ListOrdered, Quote, Link2 } from "lucide-react";
import { useKeyBindings } from "@/hooks/useKeymap";
import { cleanFragment, isBlankHtml } from "@/lib/email-html";
import { cn } from "@/lib/utils";

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
  /** Past this the editor scrolls instead of growing. */
  maxHeight?: number;
  handle?: Ref<RichTextEditorHandle>;
  /** Whether this editor's keys are live. False for a composer in the background. */
  active?: boolean;
}

export function RichTextEditor({
  docKey,
  initialHtml,
  onChange,
  disabled = false,
  placeholder = "Write your message",
  maxHeight = 340,
  handle,
  active = true,
}: RichTextEditorProps) {
  const root = useRef<HTMLDivElement>(null);
  const editor = useRef<Squire | null>(null);
  const latest = useRef(onChange);
  latest.current = onChange;

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
  }, [docKey]); // eslint-disable-line -- the body is not a prop, it is a document

  useEffect(() => {
    const node = root.current;
    if (node) node.contentEditable = disabled ? "false" : "true";
  }, [disabled]);

  useImperativeHandle(
    handle,
    () => ({
      focus: () => editor.current?.focus(),
      html: () => editor.current?.getHTML() ?? "",
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
      keys: "mod+k",
      group: "Composer",
      description: "Link",
      allowInInput: true,
      // Above the command palette's own ⌘K, and only while this composer is the
      // one being typed into — so ⌘K is still search everywhere else.
      priority: 210,
      when: () => active && !disabled,
      handler: () => (linking ? setLinking(false) : startLink()),
    },
  ]);

  return (
    <div className="mt-3">
      <div className="flex items-center gap-0.5 pb-1.5">
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
        <Tool
          label="Quote"
          keys="⌘]"
          on={inside("BLOCKQUOTE")}
          onClick={() =>
            apply((i) => (inside("BLOCKQUOTE") ? i.decreaseQuoteLevel() : i.increaseQuoteLevel()))
          }
        >
          <Quote className="size-3.5" />
        </Tool>
        <Tool label="Link" keys="⌘K" on={inside("A")} onClick={startLink}>
          <Link2 className="size-3.5" />
        </Tool>
      </div>

      {linking && (
        <div className="mb-1.5 flex items-center gap-2 border-b border-border pb-1.5">
          <span className="text-micro uppercase tracking-wide text-faint-foreground">Link</span>
          <input
            ref={linkField}
            value={linkUrl}
            spellCheck={false}
            autoComplete="off"
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

      <div className="relative">
        {empty && (
          <span className="pointer-events-none absolute left-0 top-0 text-reading leading-[1.6] text-faint-foreground">
            {placeholder}
          </span>
        )}
        <div
          ref={root}
          role="textbox"
          aria-multiline="true"
          aria-label="Message"
          spellCheck
          style={{ maxHeight }}
          className={cn(
            "block min-h-[5.5rem] w-full overflow-y-auto bg-transparent",
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
