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
   CSP in `tauri.conf.json`. A `srcdoc` document inherits its creator's policy
   container, so the two intersect. `tauri.conf.json`'s `img-src` must keep
   `https:` for "load remote images" to render anything; it has no plain
   `http:`, so an http image in old mail does not load in either mode.

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

   **The same is true of every other listener on that document, and forgetting
   it cost a third bug report.** A `keydown` listener was attached four lines
   below the click one, to hand the app back its shortcuts once focus entered a
   message — and for the same reason it has never fired in Mach. Click anywhere
   in a message body and the whole keyboard went dead: archive, star, snooze,
   the way back to the list. It was filed as "the R shortcut isn't working
   consistently", fixed, measured clean in Blink, and filed again as "after I
   click a link in an email, the E archive keycut doesn't register".

   Keys are therefore read below the engine too, in `frame_keyboard` — an
   `NSEvent` monitor in the app's own event stream, the same mechanism `scroll`
   and `browser` already use. It publishes a key while the frontend says focus
   is inside a message frame and swallows nothing, so the frame keeps what
   belongs to it: arrows, space, `⌘A` and `⌘C`.

   Moving focus out of the frame instead would have been smaller and was
   rejected: a drag-select *is* focus in the frame, so blurring it makes WebKit
   discard the selection and leaves `⌘C` nothing to copy.

   The rule to carry forward: **a listener the parent attaches to the frame's
   document is a browser-only convenience.** Anything the app must actually do
   belongs below the engine.

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

## The attack the sanitizer cannot touch

10. **A link whose text names a different site than its `href` has to say so.**

    Everything above is about stopping a message *doing* something. A phishing
    link does nothing: it is a well-formed `https:` anchor, and it survives every
    rule here intact because it has to — the reader asked to be shown their mail
    and a link is mail. What makes it work is that the reader is shown the
    anchor's text and the browser is given its `href`.

    A browser has a status bar for this. A message frame cannot: no script runs
    inside it (invariant 1) and WebKit will not run a hover listener attached
    from the parent either, for the same reason it will not run the click one.
    So the disclosure is put in the document before the reader looks at it, by
    the parent, as an element — invariant 6 applies.

    Two cases only, and the second half of that sentence is the requirement:

    - the anchor's text is itself a URL or a bare domain naming a different
      registrable domain than the `href`;
    - the `href`'s host carries a punycode label, whatever the text says, since
      a homograph renders as the name it is imitating.

    `<a href="https://links.example.net/x">Update your payment method</a>` is
    **not** disclosed. Marketing mail is nothing but redirectors behind prose,
    and flagging those teaches the reader to ignore the flag by the third
    newsletter of the morning. A link that claims nothing cannot be lying.

    `linkClaim` in `src/lib/message-body.ts` is the whole decision.

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
- A message link pointing at one of the app's own hosts — `localhost`,
  `127.0.0.1`, `*.localhost` — is not external by `is_external_link`, so
  `link_guard` lets it through instead of opening it, and nothing answers the
  new-window request it becomes. That is a dead click, which invariant 9 says
  must not happen. It is left alone because the hook cannot tell a message
  frame's navigation from the app's own, and the app's own initial load in
  development *is* `http://localhost`. Nothing worse than the dead click is
  reachable: the frame's CSP is `default-src 'none'`, so no subresource fetch to
  those hosts is possible, and every anchor carries `target="_blank"`.
- The link disclosure's registrable-domain comparison is a short suffix list,
  not the public suffix list. Getting it wrong can only cost a disclosure that
  is not made, never a false one.
- A bare-domain phish under a TLD outside `TEXT_DOMAIN_TLDS` is not disclosed.
  The list is what keeps `README.md` and `setup.sh` from being read as domains.
- Quote *attribution* detection is English-only (`On … wrote:`,
  `-----Original Message-----`). Non-English mail falls back to structural
  markers (`gmail_quote`, `blockquote type=cite`, Outlook ids), which covers
  most real mail but not all.
