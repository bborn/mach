import { useMach } from "@/hooks/useMach";
import { Button } from "@/components/ui/button";
import { SyncBar } from "@/components/chrome/SyncIndicator";

/**
 * What the list says when it has no rows.
 *
 * Four different situations reach this component and each has a different next
 * action: set an environment variable, authorize an account, wait for the first
 * pass, or nothing at all because the mailbox is genuinely empty. They are
 * deliberately not one spinner.
 */
export function MailboxNotice() {
  const { state, actions, labels, ui, progress } = useMach();
  const mailbox = labels.find((l) => l.id === ui.labelId)?.name ?? ui.labelId;

  switch (state.kind) {
    case "loading":
      return <Notice muted>Reading the local store…</Notice>;

    case "notConfigured":
      return (
        <Notice
          title="No Google credentials"
          detail={state.message}
          body={
            <>
              Mach talks straight to Google with your own OAuth client. Set{" "}
              <code className="font-mono text-micro text-foreground">MACH_GOOGLE_CLIENT_ID</code>{" "}
              (and <code className="font-mono text-micro text-foreground">MACH_GOOGLE_CLIENT_SECRET</code>{" "}
              for a desktop client) in the environment Mach launches from, then relaunch.
            </>
          }
        />
      );

    case "noAccounts":
      return (
        <Notice
          title="No accounts yet"
          body="Nothing has been authorized, so there is nothing to sync. Adding an account opens Google's consent screen in your browser."
          action={
            <Button variant="default" onClick={() => actions.setAddAccount(true)}>
              Add account
            </Button>
          }
        />
      );

    case "syncing":
      return (
        <Notice title="First sync in progress" body={FIRST_SYNC_COPY}>
          <div className="mt-3 w-full max-w-xs">
            <SyncBar progress={progress} />
            <div className="mt-1.5 font-mono text-micro tabular-nums text-faint-foreground">
              {progress.label}
            </div>
          </div>
        </Notice>
      );

    case "error":
      return (
        <Notice
          title="Sync trouble"
          detail={state.message}
          body="Mail already in the local store still opens. This is the last error the sync engine reported."
          action={
            <Button variant="subtle" onClick={actions.syncNow}>
              Try again
            </Button>
          }
        />
      );

    case "empty":
      return <Notice muted>Nothing in {mailbox}.</Notice>;

    case "ready":
      return null;
  }
}

const FIRST_SYNC_COPY =
  "The first pass pulls twelve months of mail per account — around thirteen minutes for a large one. Conversations appear here as they land; nothing has to finish first.";

interface NoticeProps {
  title?: string;
  body?: React.ReactNode;
  /** The backend's own words, kept verbatim and visually quieter. */
  detail?: string;
  action?: React.ReactNode;
  children?: React.ReactNode;
  muted?: boolean;
}

function Notice({ title, body, detail, action, children, muted }: NoticeProps) {
  if (muted) {
    return <div className="px-3 py-6 text-list text-faint-foreground">{body ?? children}</div>;
  }

  return (
    <div className="flex flex-col items-start gap-1 px-4 py-6">
      {title && <div className="text-list font-medium text-foreground">{title}</div>}
      {body && (
        <p className="max-w-[44ch] text-list leading-[1.5] text-muted-foreground">{body}</p>
      )}
      {detail && (
        <p className="max-w-[44ch] break-words font-mono text-micro leading-[1.5] text-faint-foreground">
          {detail}
        </p>
      )}
      {children}
      {action && <div className="mt-3">{action}</div>}
    </div>
  );
}
