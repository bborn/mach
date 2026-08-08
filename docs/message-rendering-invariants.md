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

3. **Navigation must be intercepted** and handed to the system browser. Rust
   cannot stop the WebView from navigating.

4. **`data-mach-blocked-src` must be consumed as a DOM property**
   (`img.src = img.dataset.machBlockedSrc`), never concatenated into an HTML
   string. The stored value is percent-encoded so even that would be inert —
   do not rely on it.

5. **CID resolution must be scoped to the current message.** The sanitizer
   validates the *shape* of `data-mach-cid`, not its ownership. A resolver that
   accepts a Content-ID from another message or account leaks across accounts.

## Behaviour

6. **Auto-expand when new content is empty.** A body that is entirely quoted is
   legitimate for a bare forward — and is also how a sender hides their whole
   message behind the collapse.

7. **Render off the UI thread.** Input is capped at 8 MiB, but a pathological
   body still costs CPU.

## Known gaps

- A `data:image/png` payload that is really something else is not
  content-sniffed. No network and no script execution in `<img>` context; the
  CSP `img-src` is the backstop.
- Quote *attribution* detection is English-only (`On … wrote:`,
  `-----Original Message-----`). Non-English mail falls back to structural
  markers (`gmail_quote`, `blockquote type=cite`, Outlook ids), which covers
  most real mail but not all.
