import { Calendar, Mail, PenLine, Video } from "lucide-react";
import {
  artifactAction,
  eventWhen,
  guestLine,
  type Artifact,
} from "@/lib/agent";
import { joinUrl } from "@/lib/calendar-links";
import { listTime } from "@/lib/time";
import { cn } from "@/lib/utils";

/**
 * What the agent surfaced, drawn as the thing it is.
 *
 * It used to be a button saying "Show event" beside a line of tool output, and
 * everything the event actually *was* arrived as prose the model retyped
 * underneath — the title, the time, the guests, the Meet link, as a bulleted
 * list nobody could click. So the object is drawn here instead, from the fields
 * the artifact carries, and the model is told (see `agent::context`) to refer to
 * it rather than transcribe it.
 *
 * # Where the register comes from
 *
 * Nothing here is invented. An event's title over its time is `EventBlock`'s
 * stacking; a conversation's sender over its subject with the date on the right
 * is `ThreadRow`'s first two lines; the icon-then-title-then-quiet-detail rhythm
 * is a ⌘K row. What is new is only that it sits inside a drawer, so it is
 * bordered and boxed rather than a full-width row in a list.
 *
 * # Two focus stops, not one
 *
 * The body opens the object where it lives — the event in the calendar scrolled
 * to its start, the conversation in the reading pane, the draft back in the
 * composer. A video call is the one field on an event people click *instead* of
 * opening it, so it gets its own button rather than being text inside the first
 * one: a nested control is invalid markup and unreachable by keyboard, and this
 * app has no mouse-only affordances. Both are ordinary buttons in the drawer's
 * own tab order, which is how its minimise, close and approval controls are
 * reached.
 */
export function ArtifactCard({
  artifact,
  onOpen,
  onOpenExternal,
}: {
  artifact: Artifact;
  onOpen: () => void;
  /** The video call. Absent in tests and in a browser tab, where it does nothing. */
  onOpenExternal?: (url: string) => void;
}) {
  const join = artifact.kind === "event" ? joinUrl(artifact.conferenceUrl) : null;

  return (
    <div
      className={cn(
        // Capped, and not because of the text: the drawer is as wide as the
        // window, and a card stretched across it puts its title at one end of
        // the screen and its Join button at the other, which reads as a banner
        // rather than as one object.
        "my-1 flex w-full max-w-xl items-stretch overflow-hidden rounded-[var(--radius)]",
        "border border-border bg-surface",
      )}
    >
      <button
        type="button"
        onClick={onOpen}
        // The verb, then the thing: a screen reader announces "Show event, 30
        // min meeting…" rather than a title with no idea what pressing it does.
        aria-label={`${artifactAction(artifact)}: ${artifact.label}`}
        className={cn(
          "flex min-w-0 flex-1 flex-col gap-0.5 px-2 py-1.5 text-left",
          "hover:bg-row-hover focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-accent",
        )}
      >
        <Head artifact={artifact} />
        <Detail artifact={artifact} />
      </button>

      {join && (
        <button
          type="button"
          onClick={() => onOpenExternal?.(join)}
          aria-label={`Join the video call for ${artifact.label}`}
          className={cn(
            "flex shrink-0 items-center gap-1 border-l border-border px-2 text-micro text-accent",
            "hover:bg-row-hover focus-visible:outline-none focus-visible:ring-1 focus-visible:ring-accent",
          )}
        >
          <Video size={11} strokeWidth={1.75} />
          Join
        </button>
      )}
    </div>
  );
}

/** Line one: what it is, and what it is called. */
function Head({ artifact }: { artifact: Artifact }) {
  const Icon = artifact.kind === "event" ? Calendar : artifact.kind === "draft" ? PenLine : Mail;
  return (
    <span className="flex min-w-0 items-center gap-1.5">
      <Icon size={11} strokeWidth={1.75} className="shrink-0 text-faint-foreground" />
      {artifact.kind === "thread" && artifact.unread && (
        <span className="h-1.5 w-1.5 shrink-0 rounded-full bg-accent" />
      )}
      <span className="min-w-0 flex-1 truncate text-list font-medium leading-tight text-foreground">
        {artifact.label}
      </span>
      {artifact.kind === "thread" && artifact.atMs != null && (
        <span className="shrink-0 font-mono text-micro tabular-nums text-faint-foreground">
          {listTime(artifact.atMs)}
        </span>
      )}
    </span>
  );
}

/**
 * Line two: the facts the model used to write out in bullets.
 *
 * One line, and only what this kind of thing has. An event with no guests says
 * nothing about guests; a draft with no recipients is just a subject.
 */
function Detail({ artifact }: { artifact: Artifact }) {
  const parts = detailParts(artifact);
  if (parts.length === 0) return null;
  return (
    <span className="min-w-0 truncate text-micro leading-tight text-faint-foreground">
      {parts.join(" · ")}
    </span>
  );
}

function detailParts(artifact: Artifact): string[] {
  if (artifact.kind === "event") {
    const guests = guestLine(artifact.guests, artifact.guestCount);
    return [
      eventWhen(artifact),
      artifact.location ?? "",
      guests,
      // Only when he has answered: "needsAction" is the absence of an answer,
      // and a card that says so is telling him something he did not ask.
      artifact.rsvp && artifact.rsvp !== "needsAction" ? artifact.rsvp : "",
    ].filter(Boolean);
  }
  if (artifact.kind === "thread") {
    return [artifact.from ?? "", artifact.accountEmail ?? ""].filter(Boolean);
  }
  const to = guestLine(artifact.to);
  return [to ? `To ${to}` : "", "Unsent"].filter(Boolean);
}
