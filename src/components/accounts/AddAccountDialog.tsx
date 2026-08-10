import { useCallback, useEffect, useRef, useState } from "react";
import { useMach } from "@/hooks/useMach";
import { useKeyBindings } from "@/hooks/useKeymap";
import { getDataSource, type MachDataSource } from "@/lib/data";
import { toMailboxError } from "@/hooks/useThreadStream";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Overlay } from "@/components/ui/dialog";

export type Phase =
  | { step: "idle" }
  | { step: "opening" }
  /** The browser is open and Rust is holding the loopback listener. */
  | { step: "waiting"; url: string }
  | { step: "done"; email: string }
  | { step: "failed"; message: string };

/** What `authorize` needs from the data source, and nothing more. */
type AuthorizationSource = Pick<
  MachDataSource,
  "beginAddAccount" | "completeAddAccount" | "openExternal"
>;

/**
 * One sign-in, start to finish, as a value rather than a sequence of setState.
 *
 * A React component is the wrong place to keep the one rule that matters here —
 * that a refused authorization ends as a message on screen and never as a
 * silently unchanged dialog — so the run lives out here where a test can drive
 * it. `email` is the address a repair is for; without one this is a new
 * account. `onWaiting` is called once the consent URL exists, because the
 * dialog has something to say while the browser has the user.
 *
 * It resolves rather than throws: every outcome is a phase to render.
 */
export async function authorize(
  source: AuthorizationSource,
  email?: string,
  onWaiting?: (url: string) => void,
): Promise<Phase> {
  try {
    const pending = await source.beginAddAccount(email);
    onWaiting?.(pending.url);
    await source.openExternal(pending.url);
    const account = await source.completeAddAccount(pending.pendingId);
    return { step: "done", email: account.email };
  } catch (caught) {
    // Silent failure is the thing this project has paid most for. A sign-in
    // Google refused, or one that came back as a different account, says so
    // where it was started.
    return { step: "failed", message: toMailboxError(caught).message };
  }
}

/**
 * Authorizing an account.
 *
 * `begin_add_account` binds a loopback listener and returns Google's consent
 * URL; the system browser takes it from there and `complete_add_account`
 * resolves when the redirect comes back. The copy about the unverified-app
 * interstitial is not decoration: Mach uses a restricted Gmail scope on an
 * unverified client, so Google shows a full-page warning that reads exactly
 * like a failure unless you were told to expect it.
 *
 * # Repairing an account is the same flow
 *
 * When an account loses its Keychain entry, Preferences → Accounts marks the
 * row and offers "Sign in again", which opens this with `ui.addAccountEmail`
 * set. The only differences are the title, the address going to Google as a
 * `login_hint`, and Rust refusing to finish if a different account comes back —
 * the handshake, the loopback listener and the token write are the same code.
 * `persist_account` upserts on the address, so the row keeps its id, its
 * colour, its mail and its sync watermarks.
 */
export function AddAccountDialog() {
  const { ui, actions, accounts } = useMach();
  const open = ui.addAccountOpen;
  const repairing = ui.addAccountEmail;
  const [phase, setPhase] = useState<Phase>({ step: "idle" });
  const live = useRef(true);

  useEffect(() => {
    live.current = true;
    return () => {
      live.current = false;
    };
  }, []);

  useEffect(() => {
    if (!open) setPhase({ step: "idle" });
  }, [open]);

  const start = useCallback(async () => {
    setPhase({ step: "opening" });
    const next = await authorize(getDataSource(), repairing ?? undefined, (url) => {
      if (live.current) setPhase({ step: "waiting", url });
    });
    if (!live.current) return;
    setPhase(next);
    // A new account changes every list in the window. A repaired one clears its
    // own "Needs authorization" the moment Rust emits the sync status that
    // `complete_add_account` sends after `mark_reauthorized`.
    if (next.step === "done") actions.reload();
  }, [actions, repairing]);

  const busy = phase.step === "opening" || phase.step === "waiting";

  /*
   * Escape, which this surface never had.
   *
   * It went unnoticed while an unclaimed key fell through to the list behind
   * the dialog — Escape did *something*, it just closed a conversation nobody
   * was looking at. Now that an overlay holds the keyboard, the key would do
   * nothing at all, and a dialog you cannot dismiss from the keyboard is the
   * one thing this app does not ship. Not while the browser is mid-consent:
   * the loopback listener is still bound and closing the window would strand
   * it, which is the same reason the backdrop declines then too.
   */
  useKeyBindings([
    {
      keys: "escape",
      priority: 125,
      allowInInput: true,
      when: () => open && !busy,
      handler: () => actions.setAddAccount(false),
    },
  ]);

  return (
    <Overlay
      open={open}
      onClose={() => {
        if (!busy) actions.setAddAccount(false);
      }}
      align="center"
      labelledBy="add-account-title"
      className="max-w-[30rem]"
    >
      <div className="flex flex-col gap-3 p-4">
        <div>
          <h2 id="add-account-title" className="text-body font-medium text-foreground">
            {repairing ? "Sign in again" : "Add a Google account"}
          </h2>
          {/*
            This carried a paragraph about Mach never seeing the password and
            the tokens going into the Keychain. Both true, both reassurance —
            and reassurance nobody asked for reads as a reason to worry. What
            is left is the only part that changes what you do next: the window
            will sit there while a browser tab does the work.

            A repair adds the address, because this surface is reached from the
            status bar as well as from the row, and "which account" is then the
            one thing the title cannot say.
          */}
          <p className="mt-1 text-list leading-[1.5] text-muted-foreground">
            {repairing && (
              <>
                <span className="text-foreground">{repairing}</span>.{" "}
              </>
            )}
            Consent opens in your browser.
          </p>
        </div>

        <Expectation />

        {phase.step === "waiting" && (
          <p className="text-list text-muted-foreground">
            Finish in your browser, or{" "}
            <button
              type="button"
              onClick={() => void getDataSource().openExternal(phase.url)}
              className="text-accent hover:underline"
            >
              open the consent page
            </button>
            .
          </p>
        )}

        {phase.step === "done" &&
          (repairing ? (
            <p className="text-list text-foreground">
              Signed in as <span className="font-medium">{phase.email}</span>.
            </p>
          ) : (
            <p className="text-list text-foreground">
              Added <span className="font-medium">{phase.email}</span>. First sync takes a few
              minutes.
            </p>
          ))}

        {phase.step === "failed" && (
          <p className="max-w-full break-words font-mono text-micro leading-[1.5] text-danger">
            {phase.message}
          </p>
        )}

        <div className="mt-1 flex items-center gap-2">
          {phase.step === "done" ? (
            <Button variant="default" onClick={() => actions.setAddAccount(false)}>
              Done
            </Button>
          ) : (
            <Button variant="default" disabled={busy} onClick={() => void start()}>
              {phase.step === "failed" ? "Try again" : busy ? "Waiting" : "Continue"}
            </Button>
          )}
          <Button variant="ghost" disabled={busy} onClick={() => actions.setAddAccount(false)}>
            Cancel
          </Button>
          <span
            className={cn(
              "ml-auto font-mono text-micro tabular-nums text-faint-foreground",
              accounts.length === 0 && "invisible",
            )}
          >
            {accounts.length} connected
          </span>
        </div>
      </div>
    </Overlay>
  );
}

/**
 * The warning about the warning. Mach is published but unverified, so Google
 * shows "Google hasn't verified this app" on the first authorization of each
 * account. Clicking Advanced is the expected path, once per account.
 */
function Expectation() {
  return (
    <ol className="flex flex-col gap-1 rounded-[var(--radius)] border border-border bg-surface-raised p-3 text-list leading-[1.5] text-muted-foreground">
      <Step n={1}>Pick the account.</Step>
      <Step n={2}>
        Google will say <span className="text-foreground">“Google hasn’t verified this app”</span>.
        That is expected.
      </Step>
      <Step n={3}>
        Click <span className="text-foreground">Advanced</span>, then{" "}
        <span className="text-foreground">Go to Mach (unsafe)</span>, then allow the Gmail and
        Calendar scopes.
      </Step>
    </ol>
  );
}

function Step({ n, children }: { n: number; children: React.ReactNode }) {
  return (
    <li className="flex gap-2">
      <span className="shrink-0 font-mono text-micro tabular-nums text-faint-foreground">{n}</span>
      <span className="min-w-0">{children}</span>
    </li>
  );
}
