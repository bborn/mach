# Plugin sandbox — proof of concept

This exists to answer one question, which is the only genuinely risky claim in
[`../plugins.md`](../plugins.md):

> **Does a plugin running in an iframe on its own origin, under
> `connect-src 'none'`, actually lose the network, the app's DOM, the app's
> storage and the Tauri IPC surface — in WKWebView on macOS, not just in
> Chrome?**

If the answer is no, the tier-1 design collapses and the fallback is WASM. So
this is written first, and step 0 of the implementation order is "make this pass
in the real app".

It is not a plugin system. It is a harness plus a canary — and it has already
paid for itself twice, which is the case for having built it. See
[What it caught](#what-it-caught).

## Run it

In a plain browser (verifies most of it):

```sh
cd docs/plugin-poc
python3 -m http.server 8765
open http://localhost:8765/
```

One server, two origins: the harness loads from `localhost` and the guest from
`127.0.0.1`, which the browser treats as a different origin. In the real app
that second origin is a Tauri custom protocol, `plugin://<id>/`.

In the real app (verifies the part that matters): serve this directory through
the dev server and open the same page in a Mach window. The numbers that count
are the ones WKWebView produces.

## What you should see

Three panels.

**Escape attempts** — twenty things a hostile plugin would try, split between
the guest document and the worker inside it. Every row must say **BLOCKED**.
One `ALLOWED` row is a design failure, and the row names which one.

| Scope | Attempt | Must be blocked by |
|---|---|---|
| guest | `fetch` a remote URL | CSP `connect-src 'none'` |
| guest | `fetch` the app's origin | CSP `connect-src 'none'` |
| guest | being on the app's origin at all | the custom protocol / distinct host |
| guest | read the app's `localStorage` (planted sentinel) | separate storage partition |
| guest | read the app's cookies (planted sentinel) | separate storage partition |
| guest | `parent.document` | cross-origin barrier |
| guest | `globalThis.__TAURI_INTERNALS__` | nothing injects it |
| guest | remove its own `sandbox` attribute | cross-origin barrier |
| worker | `fetch` remote, `fetch` app origin | CSP `connect-src` |
| worker | `XMLHttpRequest`, `WebSocket`, `EventSource`, `sendBeacon` | CSP `connect-src` |
| worker | `importScripts` / `import()` of a remote URL | CSP `script-src` |
| worker | `document`, `localStorage`, `parent`, `__TAURI_INTERNALS__` | worker scope |

**Channel** — a small example plugin runs and gets a result back through the
capability-checked host API, proving the boundary is not so tight that nothing
useful crosses it. It also shows a capability *denial*: the plugin asks for a
`trash` command it did not declare, and the host refuses it by name.

**Negative result** — the same guest in an opaque-origin `srcdoc` frame, which
is where this design started and which does not work. Kept so the finding stays
reproducible instead of becoming folklore.

## What it caught

Three things, all before a line of host code was written for real.

1. **An opaque origin cannot host a Worker.** `new Worker(blobUrl)` constructs
   without complaint in a `sandbox="allow-scripts"` frame and then immediately
   fails to start: the URL is `blob:null/…`, and nothing can fetch that. The
   first draft of the design was unimplementable as written, and its failure
   mode was a silent five-second timeout rather than an error. Hence the real
   origin, and hence `t: "fatal"` in the protocol.
2. **One origin for all plugins is one storage partition for all plugins.** The
   first version of the storage probe reported `localStorage` as ALLOWED, which
   was true and not the point — on a distinct origin it exists and is empty.
   Rewriting the probe to plant a sentinel in the *app's* storage and look for
   it made the test meaningful, and made the remaining risk obvious: plugins
   sharing one guest origin would share one store. Hence `plugin://<id>/`.
3. **Testing a constructor is not testing a connection.** `new EventSource(…)`
   does not throw when CSP will refuse the connection; it constructs and fails
   asynchronously. The probe had to wait for `open` or `error`. A conformance
   suite that gets this wrong passes when it should fail, which is the worst
   thing a conformance suite can do.

## The architecture being tested

```
app window (React, Tauri IPC, all the mail)        origin: the app's
│
│  postMessage, structured clone only
▼
<iframe sandbox="allow-scripts allow-same-origin"> origin: plugin://<id>
   guest.html, CSP: connect-src 'none'
   │
   │  postMessage
   ▼
   Worker (module, from a blob: URL)               inherits the guest's CSP
      plugin main.js runs here
```

`allow-same-origin` is correct here and is *not* the combination
`docs/message-rendering-invariants.md` forbids. That rule is about content on
the app's own origin, where the pair lets a frame reach app storage and strip
its own sandbox attribute. Here the guest's own origin is already foreign, so
the flag means "keep being foreign" — and it has to be granted, because an
opaque origin cannot run a worker (finding 1). The conformance test asserts the
distinction directly rather than trusting this paragraph.

`sandbox.js` is written to be read: it is the guest side of the channel, and it
is about a hundred lines.

## Files

| File | What it is |
|---|---|
| `index.html` | The harness page and its three result panels |
| `host.js` | The host: creates the guest, enforces capabilities, answers plugin calls |
| `guest.html` | The guest document, on its own origin, carrying the CSP |
| `sandbox.js` | The guest's script: a relay, plus the document-scope probes |
| `worker.js` | The worker shim that imports the plugin and proxies `mach.*` |
| `plugins/canary.js` | Tries every worker-scope escape and reports |
| `plugins/example.js` | A real, tiny plugin: reads labels, runs a command, gets denied one |

In the real app, `guest.html` and `sandbox.js` are served by the protocol
handler with the CSP as a **response header** rather than a `<meta>` tag — one
fewer file a plugin could ever influence.

## Answered: WKWebView, 2026-08-08

Step 0 passed. The same checks, plus a positive control and an `<img>` probe,
now run inside a real Mach window against the real `plugin://` protocol handler
— **22 of 22 blocked**, guest origin `plugin://conformance`. The measurements
are in `docs/plugins.md` §2 and the harness is
`src-tauri/src/bin/plugin_probe.rs`, which is a hidden window with the macOS
activation policy set to `Accessory`, so running it costs nobody their keyboard:

```sh
MACH_DATA_DIR=.qa/plugin-probe/data cargo run --bin plugin_probe
```

This directory stays as the Chrome baseline and as the record of what the PoC
caught. The living conformance test is `src/lib/plugins/conformance.ts`, which
runs at plugin-host boot, every boot.

## What this deliberately does not prove

- **WebView2.** Has to be checked separately before Mach ships on Windows.
- **Tier 2.** Subprocess plugins have a network by design; nothing here applies.
- **Denial of service.** A plugin can still spin. The worker keeps it off the UI
  thread and the host's per-call timeout abandons the call, but a runaway worker
  needs terminating, which is not modelled here.
- **That the API is nice.** That is what the worked examples in `plugins.md` are
  for.
