import { useCallback, useEffect, useRef, useState } from "react";
import { useMach } from "@/hooks/useMach";
import { useKeyBindings } from "@/hooks/useKeymap";
import { getDataSource } from "@/lib/data";
import { toMailboxError } from "@/hooks/useThreadStream";
import { cn } from "@/lib/utils";
import { Button } from "@/components/ui/button";
import { Overlay } from "@/components/ui/dialog";

type Phase =
  | { step: "idle" }
  | { step: "opening" }
  /** The browser is open and Rust is holding the loopback listener. */
  | { step: "waiting"; url: string }
  | { step: "done"; email: string }
  | { step: "failed"; message: string };

/**
 * Authorizing an account.
 *
 * `begin_add_account` binds a loopback listener and returns Google's consent
 * URL; the system browser takes it from there and `complete_add_account`
 * resolves when the redirect comes back. The copy about the unverified-app
 * interstitial is not decoration: Mach uses a restricted Gmail scope on an
 * unverified client, so Google shows a full-page warning that reads exactly
 * like a failure unless you were told to expect it.
 */
export function AddAccountDialog() {
  const { ui, actions, accounts } = useMach();
  const open = ui.addAccountOpen;
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
    const source = getDataSource();
    try {
      const pending = await source.beginAddAccount();
      if (!live.current) return;
      setPhase({ step: "waiting", url: pending.url });
      await source.openExternal(pending.url);
      const account = await source.completeAddAccount(pending.pendingId);
      if (!live.current) return;
      setPhase({ step: "done", email: account.email });
      // A new account changes every list in the window.
      actions.reload();
    } catch (caught) {
      if (!live.current) return;
      setPhase({ step: "failed", message: toMailboxError(caught).message });
    }
  }, [actions]);

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
            Add a Google account
          </h2>
          {/*
            This carried a paragraph about Mach never seeing the password and
            the tokens going into the Keychain. Both true, both reassurance —
            and reassurance nobody asked for reads as a reason to worry. What
            is left is the only part that changes what you do next: the window
            will sit there while a browser tab does the work.
          */}
          <p className="mt-1 text-list leading-[1.5] text-muted-foreground">
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

        {phase.step === "done" && (
          <p className="text-list text-foreground">
            Added <span className="font-medium">{phase.email}</span>. First sync takes a few
            minutes.
          </p>
        )}

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
