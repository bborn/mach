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
 *
 * Each of these used to carry a paragraph explaining itself — that Mach talks
 * straight to Google with your own OAuth client, that nothing has been
 * authorized so there is nothing to sync, that the first pass pulls twelve
 * months per account and conversations land as they arrive. All true, all
 * reasoning, and none of it is what someone staring at an empty list is
 * reading for. What survives is the state's name, the one thing they could not
 * work out for themselves (that the first sync is minutes, not seconds), and
 * the button. The reasoning lives here instead.
 */
export function MailboxNotice() {
  const { state, actions, labels, ui, progress } = useMach();
  const mailbox = labels.find((l) => l.id === ui.labelId)?.name ?? ui.labelId;

  switch (state.kind) {
    case "loading":
      return <Notice muted>Loading</Notice>;

    case "notConfigured":
      return (
        <Notice
          title="No Google credentials"
          detail={state.message}
          body={
            <>
              Set{" "}
              <code className="font-mono text-micro text-foreground">MACH_GOOGLE_CLIENT_ID</code>{" "}
              (and <code className="font-mono text-micro text-foreground">MACH_GOOGLE_CLIENT_SECRET</code>{" "}
              for a desktop client), then relaunch.
            </>
          }
        />
      );

    case "noAccounts":
      return (
        <Notice
          title="No accounts"
          action={
            <Button variant="default" onClick={() => actions.setAddAccount(true)}>
              Add account
            </Button>
          }
        />
      );

    case "syncing":
      return (
        <Notice title="First sync" body={FIRST_SYNC_COPY}>
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
          title="Sync failed"
          detail={state.message}
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

/**
 * The one thing here that is not self-evident, and so the one thing that stays.
 *
 * Twelve months per account is roughly thirteen minutes on a large mailbox.
 * Without a number, an empty inbox for that long reads as broken rather than as
 * working — which is the only reason this state says anything at all.
 */
const FIRST_SYNC_COPY = "Twelve months per account — a few minutes. Mail appears as it lands.";

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
