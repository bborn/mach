# Message rendering invariants

The Rust sanitizer (`src-tauri/src/render/`) is one half of a two-layer defence.
It cannot enforce any of the following, because they live in the WebView. A
reading pane that skips them is unsafe no matter how good the sanitizer is.

Every message body is HTML written by a stranger, rendered in an app holding
five mailboxes. Assume the sender knows exactly how the sanitizer works.

## The iframe

1. **Sandboxed, with no `allow-scripts`.** Never combine `allow-scripts` with
   `allow-same-origin` — together they let the frame reach out of the sandbox.
   The sanitizer emits no script; the sandbox is what survives the next ammonia
   CVE.

   The frame is `allow-same-origin allow-popups`. `allow-popups` is required,
   not incidental: it is the only way a click on a link reaches anything
   outside the web engine — see invariant 3. It grants nothing on a frame that
   cannot run scripts, since without scripting the only way to open a popup is
   a person clicking an anchor. `allow-popups-to-escape-sandbox` is a different
   flag and must never be added.

2. **Content-Security-Policy**, at minimum:

   ```
   default-src 'none'; script-src 'none'; base-uri 'none';
   form-action 'none'; frame-src 'none'; object-src 'none';
   style-src 'unsafe-inline'; img-src data: <attachment-scheme>:
   ```

   Widen `img-src` to `https:` *only* while the user has opted into remote
   images for that message. `style-src 'unsafe-inline'` is unavoidable — email
   is inline styles — which is exactly why the CSS scrubber exists underneath.

   Note this is the *message frame* policy, and is stricter than the app-level
   CSP in `tauri.conf.json`.

## Handling sanitizer output

3. **Navigation must be intercepted** and handed to the system browser.

   This used to say "Rust cannot stop the WebView from navigating", and that
   was the mistake. Rust can, and on macOS it is the only thing that can.

   The obvious implementation — the parent attaches a capture-phase `click`
   listener to the frame's document — works in Blink and does nothing at all in
   WebKit, which is the engine behind every macOS WebView. WebKit will not
   invoke a listener whose target document has scripting disabled, and
   invariant 1 disables scripting in that document by definition. The listener
   attaches, reports success, and never fires. Nothing logs it. A link in a
   message was dead for months and the bug was reported twice, because every
   investigation started from the assumption that this listener ran.

   Measured directly against WKWebView, on a real message, with a real mouse
   event: with `allow-same-origin` alone the parent's listener recorded nothing
   for a click on a link, and no navigation reached the app either — the
   sanitizer forces `target="_blank"` onto every anchor, and WebKit refuses a
   `_blank` navigation from a frame without `allow-popups` before anything
   outside the engine is consulted. Removing `target` instead does not help: it
   becomes a same-frame navigation, which the *app's* `frame-src` policy
   refuses, again before anything can see it.

   So interception lives at the navigation layer, in
   `ipc::render::link_guard`, which is a Tauri plugin `on_navigation` hook —
   below the web engine, consulted for subframes and new-window navigations
   alike, and not silenceable by a sandbox flag. It cancels anything external
   and opens it in the system browser. Cancelling there also happens *before*
   the engine asks anything for a window, so a message's page is never rendered
   inside Mach even briefly, and nothing in the app answers such a request in
   any case.

   The WebView-side listener stays, because `bun run dev` renders the same
   frontend in a browser tab where it does run and is the only thing there. It
   is not what makes links work in the app.

4. **`data-mach-blocked-src` must be consumed as a DOM property**
   (`img.src = img.dataset.machBlockedSrc`), never concatenated into an HTML
   string. The stored value is percent-encoded so even that would be inert —
   do not rely on it.

5. **CID resolution must be scoped to the current message.** The sanitizer
   validates the *shape* of `data-mach-cid`, not its ownership. A resolver that
   accepts a Content-ID from another message or account leaks across accounts.

6. **The parent may restructure the frame's DOM, but never by writing markup.**
   `allow-same-origin` lets the parent reach into the frame, and it uses that:
   revealing blocked images, and putting content too wide for the pane inside
   its own scroller (`containWideContent`). Every such change must be made by
   creating elements and moving nodes — `createElement`, `insertBefore`,
   `appendChild`. Never `innerHTML`, `insertAdjacentHTML` or anything else that
   re-parses a string, because the strings in there are the sender's and the
   sanitizer has already had its one look at them.

## Behaviour

7. **Auto-expand when new content is empty.** A body that is entirely quoted is
   legitimate for a bare forward — and is also how a sender hides their whole
   message behind the collapse.

8. **Render off the UI thread.** Input is capped at 8 MiB, but a pathological
   body still costs CPU.

9. **A link that could not be opened has to say so.** Every layer of this path
   failed in silence at some point: a click with no listener, a listener with
   no URL, an `openUrl` that rejected into a `console.warn`, a custom event
   nothing listened for. `reportLinkFailure` and the `link-failed` event from
   Rust both end at `LinkFailures`, which puts it in the toast. A dead click is
   not an acceptable failure mode — it is what hid invariant 3 for months.

## Known gaps

- A `data:image/png` payload that is really something else is not
  content-sniffed. No network and no script execution in `<img>` context; the
  CSP `img-src` is the backstop.
- Quote *attribution* detection is English-only (`On … wrote:`,
  `-----Original Message-----`). Non-English mail falls back to structural
  markers (`gmail_quote`, `blockquote type=cite`, Outlook ids), which covers
  most real mail but not all.
