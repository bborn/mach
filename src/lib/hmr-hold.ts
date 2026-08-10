/**
 * Which hot updates reach a running window, and when.
 *
 * Mach is developed while it is being used as a mail client. Agents commit into
 * the same checkout the owner's window is served from, so every frontend edit
 * arrives in the middle of whatever he was doing — sometimes as a module swap,
 * sometimes as a full page reload that throws away the open conversation, the
 * scroll position, the selection and the composer he had open. The code is
 * fine. The timing is his to choose.
 *
 * So the dev server stops sending updates the moment it has them. It says that
 * something is waiting, and sends it when the window asks — which is what the
 * toast's ⌘R does. Everything here is the decision half of that: what may pass,
 * what waits, and what "take it" turns into. It runs on the server
 * (`holdUpdates` in `vite.config.ts`) and is imported by the window only for
 * the event names.
 *
 * # Why the server and not the client
 *
 * `import.meta.hot.on("vite:beforeUpdate")` and `"vite:beforeFullReload"` look
 * like the interception point and are not: Vite's client dispatches those
 * listeners and then applies the update regardless. `beforeFullReload` in
 * particular is followed by `location.reload()` on the next line, and a
 * listener cannot cancel it. The only place an update can be *withheld* rather
 * than merely announced is before it goes down the socket.
 *
 * One choke point does it. Every server-initiated reload in Vite 7 —
 * `updateModules`, an edited `index.html`, a changed `tsconfig.json`, the
 * dependency optimiser deciding it needs a re-bundle — ends at
 * `environment.hot.send`. Wrapping that catches all of them, including the ones
 * no plugin hook is offered for.
 *
 * # Why taking an update is a reload, and not the module swap Vite had planned
 *
 * The first version of this kept the withheld `update` payloads and replayed
 * them on ⌘R, so a React component swap would keep the window's state. It
 * blanked the app, repeatably, and the reason is worth writing down.
 *
 * Vite's module update is not one message. The client applies what it is sent,
 * discovers a module that cannot Fast Refresh — this codebase has many, because
 * a file exporting both a component and a helper is not refreshable, and
 * `Toast.tsx` and `useMach.tsx` are both that — calls `import.meta.hot
 * .invalidate()`, and the server propagates further and sends *another* update.
 * A held release starts that conversation and then holds the server's next
 * turn, which leaves the graph half swapped: `MachProvider` replaced, its
 * subtree not, and an empty page.
 *
 * Passing everything through for a moment after a release would paper over it
 * and would also punch a hole in the one promise this makes — that nothing
 * arrives unasked. A reload has none of that: it is atomic, it is what Vite
 * would have done for most of these changes anyway, and it is the word he used.
 *
 * # What it cannot hold back
 *
 * A dev server *restart* — `vite.config.ts` or a `.env` file changing, or the
 * process being restarted by hand. The socket closes, and Vite's client reloads
 * the page by itself when it reconnects. That decision is made inside the
 * client bundle, after the server this plugin lives in has already gone.
 *
 * And nothing on the Rust side: a Tauri rebuild relaunches the binary, which is
 * a different disruption with a different answer.
 *
 * # Styles are not held
 *
 * A stylesheet swap re-paints and loses nothing — no module is re-executed, no
 * component re-mounts, no state is dropped. Holding it back would mean a toast
 * for every colour tweak, offering to interrupt him in order to prevent an
 * interruption that never happened. So style updates go straight through and
 * everything else waits.
 *
 * The cost of that split is real and small: a commit touching a component *and*
 * a stylesheet lands its stylesheet immediately and its markup when asked, so
 * for that interval the window can be painting new rules over old markup.
 * Tailwind's generated sheet is the common case and it only ever *gains*
 * classes that nothing is using yet.
 */

/* -------------------------------------------------------------------------- */
/* The payloads                                                                */
/* -------------------------------------------------------------------------- */

/**
 * One entry in Vite's `update` payload.
 *
 * Three fields of the six, because these are the three anything here reads.
 * Entries that pass are forwarded as the objects Vite built — never copied — so
 * the fields not named here survive untouched.
 */
export interface HotUpdate {
  type: string;
  path: string;
  acceptedPath: string;
}

export interface UpdatePayload {
  type: "update";
  updates: readonly HotUpdate[];
}

export interface FullReloadPayload {
  type: "full-reload";
  path?: string;
}

/** The two kinds of message that disrupt a running window. */
export type HeldPayload = UpdatePayload | FullReloadPayload;

/** True for the payloads this module has an opinion about. */
export function isHeldKind(payload: { type: string }): payload is HeldPayload {
  return payload.type === "update" || payload.type === "full-reload";
}

/** What the window is sent when it asks for what is waiting. */
export const TAKE_PAYLOAD: FullReloadPayload = { type: "full-reload", path: "*" };

/* -------------------------------------------------------------------------- */
/* The custom channel                                                          */
/* -------------------------------------------------------------------------- */

/** Server → window: there is, or is no longer, something waiting. */
export const HELD_EVENT = "mach:update-held";

/** Window → server: send me what you are holding. */
export const TAKE_EVENT = "mach:update-take";

/**
 * Window → server: I have just loaded, so whatever you were holding is already
 * in me.
 */
export const HELLO_EVENT = "mach:update-hello";

/** The body of `HELD_EVENT`. */
export interface HeldNotice {
  waiting: boolean;
}

/* -------------------------------------------------------------------------- */
/* The decision                                                                */
/* -------------------------------------------------------------------------- */

const STYLE_FILE = /\.(css|pcss|postcss|scss|sass|less|styl|stylus)(\?|$)/;

/**
 * Whether an update only changes how the page is painted.
 *
 * Two tests rather than one because the answer arrives two ways. Vite labels a
 * boundary it knows to be a stylesheet `css-update`; a stylesheet imported from
 * a module instead comes through as a `js-update` whose accepted path is the
 * `.css` file, and re-executing that module swaps a style element and nothing
 * else. Both are the same non-event to somebody reading their mail.
 */
export function isStyleUpdate(update: HotUpdate): boolean {
  return (
    update.type === "css-update" ||
    STYLE_FILE.test(update.acceptedPath) ||
    STYLE_FILE.test(update.path)
  );
}

/** What to send now, and whether anything was kept back. */
export interface Verdict {
  send: HeldPayload | null;
  /** True when the window must be told there is something waiting for it. */
  keep: boolean;
}

export function classify(payload: HeldPayload): Verdict {
  if (payload.type === "full-reload") return { send: null, keep: true };

  const styles = payload.updates.filter(isStyleUpdate);
  return {
    send: styles.length > 0 ? { type: "update", updates: styles } : null,
    keep: styles.length < payload.updates.length,
  };
}
