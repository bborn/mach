import type { CSSProperties, PointerEvent as ReactPointerEvent } from "react";
import type { CalendarEvent } from "@/types";
import type { ResizeEdge } from "@/lib/calendar-drag";
import {
  BLOCK_RADIUS,
  blockPlan,
  type BlockPlan,
} from "@/lib/calendar-geometry";
import { paintFor, type EventTone, type HueIndex } from "@/lib/calendar-palette";
import { shortTime } from "@/lib/time";
import { cn } from "@/lib/utils";

export interface EventBlockProps {
  event: CalendarEvent;
  hue: HueIndex;
  dark: boolean;
  tone: EventTone;
  past: boolean;
  selected: boolean;
  /** How many stored events this block stands for. >1 draws the merge mark. */
  copies: number;
  /** Rendered height in px — the whole density ladder keys off this. */
  height: number;
  /** Rendered width in px. Under ~120px only the start time fits. */
  width: number;
  /** True while this block is the one being dragged — the ghost has it now. */
  dimmed?: boolean;
  /**
   * Whether the edges offer to resize. False for a block the day boundary has
   * clipped, or one standing in for the same meeting on several calendars:
   * its edges are not the event's edges, so dragging them would lie.
   */
  resizable?: boolean;
  style: CSSProperties;
  onSelect: () => void;
  /** Pointer went down on the body, or on one of the two edge handles. */
  onGrab?: (
    event: ReactPointerEvent,
    kind: "move" | "resize",
    edge: ResizeEdge,
  ) => void;
  blockRef?: (node: HTMLButtonElement | null) => void;
  onPointerEnter?: () => void;
  onPointerLeave?: () => void;
}

/**
 * How much of a block the edge handles claim.
 *
 * Six pixels is Google's, and it is the largest number that still leaves the
 * middle of a 24px (30-minute) block draggable as a move. Below 24px there is
 * no room for three zones at all, so short blocks are move-only and are
 * resized from the keyboard or the modal instead.
 */
const HANDLE_PX = 6;
const HANDLE_MIN_HEIGHT = 24;

/**
 * One timed event.
 *
 * The interesting part is what happens as the block gets shorter (§5): each
 * threshold drops exactly one thing, and below 24px the title and time merge
 * into one comma-joined line — "Standup, 11am" — which reads as language and
 * truncates gracefully, because the time is what tells two similar blocks
 * apart.
 *
 * At the bottom of the ladder the text line is *taller than the block* and is
 * allowed to say so. A 15-minute event is 11px of colour with a 15px line
 * centred on it, spilling 2px top and bottom. That is Google's trick and it is
 * the reason a quarter-hour event there is always readable while everyone
 * else's is a sliver of a letterform.
 */
export function EventBlock({
  event,
  hue,
  dark,
  tone,
  past,
  selected,
  copies,
  height,
  width,
  dimmed = false,
  resizable = false,
  style,
  onSelect,
  onGrab,
  blockRef,
  onPointerEnter,
  onPointerLeave,
}: EventBlockProps) {
  const plan = blockPlan(height, { hasLocation: Boolean(event.location) });
  const paint = paintFor(hue, tone, { dark, past });
  // A half-width block cannot hold "1:30p – 2:15p" without ellipsising the one
  // part that must never ellipsise. Below ~120px, show the start only.
  const time =
    plan.inlineTime || width < 120
      ? shortTime(event.start)
      : `${shortTime(event.start)} – ${shortTime(event.end)}`;

  const showHandles = resizable && onGrab !== undefined && height >= HANDLE_MIN_HEIGHT;

  /*
   * The one place §1 and §6 collide.
   *
   * §6 draws an unanswered invitation as a white block with a 1px border in the
   * calendar's colour. §1 draws a 15-minute event as an 11px block whose 15px
   * line of text deliberately spills out of it. Put together, the block's
   * bottom edge lands on the text's baseline and the title reads as **struck
   * through** — which in this palette is what *declined* looks like. An invite
   * you have not answered was showing up as one you had refused.
   *
   * A sliver therefore keeps the white fill and the hue, and spends it on a 2px
   * bar down the left edge and on the text itself, rather than on a ring that
   * crosses the words. Nothing new is introduced and nothing is drawn through
   * the one line the block has.
   */
  const sliverOutline = plan.overflow && paint.border !== undefined;
  const ring = sliverOutline
    ? `inset 2px 0 0 0 ${paint.border}`
    : paint.border
      ? `inset 0 0 0 1px ${paint.border}`
      : undefined;

  return (
    <button
      type="button"
      ref={blockRef}
      // `tabIndex={-1}` keeps blocks out of the browser's own tab order: Tab
      // steps event-to-event through the keymap, in start order, which is the
      // order the week reads in — not the order the DOM happens to be in.
      tabIndex={-1}
      data-selected={selected || undefined}
      onClick={onSelect}
      onPointerDown={(pointer) => {
        // Anything with its own handler (the two edges) has already stopped
        // this; reaching here means the body was grabbed.
        onGrab?.(pointer, "move", "end");
      }}
      onPointerEnter={onPointerEnter}
      onPointerLeave={onPointerLeave}
      title={`${event.title} · ${shortTime(event.start)}–${shortTime(event.end)}${
        event.location ? ` · ${event.location}` : ""
      }`}
      className={cn(
        "absolute text-left transition-[left,width,box-shadow,opacity] duration-[120ms] ease-out",
        plan.overflow ? "overflow-visible" : "overflow-hidden",
        // Selection is a ring on the outside; `:focus-visible` in globals.css
        // draws its outline on the inside. Two different marks, so "the cursor
        // is here" and "the browser focus is here" never read as one thing.
        selected && "ring-2 ring-accent ring-offset-1 ring-offset-background",
        onGrab && "cursor-grab active:cursor-grabbing",
      )}
      style={{
        borderRadius: BLOCK_RADIUS,
        background: paint.background,
        color: paint.color,
        // The dragged block stays put and fades: the ghost is the thing under
        // the pointer, and the hole it left is useful context for where it was.
        opacity: dimmed ? 0.35 : paint.opacity,
        boxShadow: ring,
        // A dashed border cannot be faked with an inset shadow; tentative
        // events get a real one, inset so it does not change the geometry.
        // A sliver has no room for one either — see `sliverOutline` above.
        outline:
          paint.borderStyle === "dashed" && !sliverOutline
            ? `1px dashed ${paint.border}`
            : undefined,
        outlineOffset: paint.borderStyle === "dashed" && !sliverOutline ? -1 : undefined,
        padding: plan.tier === "full" || plan.tier === "twoLine" ? "2px 6px" : 0,
        touchAction: onGrab ? "none" : undefined,
        ...style,
      }}
    >
      {plan.overflow ? (
        <SliverLine
          title={event.title}
          time={time}
          plan={plan}
          paint={paint.timeColor}
          // On an outlined sliver the hue is the whole signal, so it carries the
          // title too — there is no border left to carry it.
          titleColor={sliverOutline ? paint.timeColor : undefined}
          strike={paint.strikethrough}
        />
      ) : (
        <Stacked
          event={event}
          time={time}
          plan={plan}
          timeColor={paint.timeColor}
          strike={paint.strikethrough}
        />
      )}

      {showHandles && (
        <>
          <ResizeHandle edge="start" onGrab={onGrab} />
          <ResizeHandle edge="end" onGrab={onGrab} />
        </>
      )}

      {copies > 1 && <MergeMark count={copies} />}
    </button>
  );
}

/**
 * The 6px strip at the top or bottom of a block.
 *
 * Invisible until hovered — a permanently-drawn handle on every block in a
 * dense week is visual noise for an affordance you only want when you are
 * already there.
 */
function ResizeHandle({
  edge,
  onGrab,
}: {
  edge: ResizeEdge;
  onGrab: (event: ReactPointerEvent, kind: "move" | "resize", edge: ResizeEdge) => void;
}) {
  return (
    <span
      role="presentation"
      onPointerDown={(pointer) => {
        pointer.stopPropagation();
        onGrab(pointer, "resize", edge);
      }}
      className={cn(
        "group/handle absolute inset-x-0 z-10 cursor-ns-resize",
        edge === "start" ? "top-0" : "bottom-0",
      )}
      style={{ height: HANDLE_PX }}
    >
      <span
        className="absolute left-1/2 h-[2px] w-5 -translate-x-1/2 rounded-full bg-current opacity-0 transition-opacity duration-100 group-hover/handle:opacity-80"
        style={{ top: (HANDLE_PX - 2) / 2 }}
      />
    </span>
  );
}

/**
 * The ≤15-minute case. The line is absolutely positioned and vertically centred
 * on the block, so an 11px block still shows a full 15px line of text.
 */
function SliverLine({
  title,
  time,
  plan,
  paint,
  titleColor,
  strike,
}: {
  title: string;
  time: string;
  plan: BlockPlan;
  paint: string;
  titleColor?: string;
  strike?: boolean;
}) {
  return (
    <span
      className="pointer-events-none absolute inset-x-0 flex items-center gap-1 overflow-hidden px-1.5"
      style={{
        top: "50%",
        transform: "translateY(-50%)",
        height: plan.lineHeightPx,
        lineHeight: `${plan.lineHeightPx}px`,
        fontSize: plan.fontPx,
        whiteSpace: "nowrap",
        textOverflow: "clip",
      }}
    >
      <span
        className={cn("min-w-0 shrink overflow-hidden font-medium", strike && "line-through")}
        style={{ textOverflow: "ellipsis", color: titleColor }}
      >
        {title},
      </span>
      <span className="shrink-0 tabular-nums" style={{ color: paint }}>
        {time}
      </span>
    </span>
  );
}

function Stacked({
  event,
  time,
  plan,
  timeColor,
  strike,
}: {
  event: CalendarEvent;
  time: string;
  plan: BlockPlan;
  timeColor: string;
  strike: boolean;
}) {
  const lineStyle: CSSProperties = {
    fontSize: plan.fontPx,
    lineHeight: `${plan.lineHeightPx}px`,
  };

  // One line, comma-joined. The title ellipsises; the time never does, because
  // the time is what disambiguates two blocks with similar names.
  if (plan.inlineTime) {
    return (
      <span className="flex items-center gap-1 px-1.5" style={lineStyle}>
        <span className={cn("min-w-0 shrink truncate font-medium", strike && "line-through")}>
          {event.title},
        </span>
        <span className="shrink-0 tabular-nums" style={{ color: timeColor }}>
          {time}
        </span>
      </span>
    );
  }

  return (
    <span className="block" style={lineStyle}>
      <span
        className={cn(
          "block font-medium",
          plan.wrapTitle ? "line-clamp-2" : "truncate",
          strike && "line-through",
        )}
      >
        {event.title}
      </span>
      <span className="block truncate tabular-nums" style={{ color: timeColor, opacity: 0.85 }}>
        {time}
      </span>
      {plan.showLocation && (
        <span className="block truncate" style={{ color: timeColor, opacity: 0.85 }}>
          {event.location}
        </span>
      )}
    </span>
  );
}

/** Two offset rounded rects: "this block is the same meeting on N calendars". */
function MergeMark({ count }: { count: number }) {
  return (
    <span
      className="pointer-events-none absolute right-[3px] top-[3px] opacity-80"
      title={`On ${count} calendars`}
      aria-label={`On ${count} calendars`}
    >
      <svg width="8" height="8" viewBox="0 0 8 8" fill="none" aria-hidden="true">
        <rect x="0.5" y="2.5" width="5" height="5" rx="1.5" stroke="currentColor" />
        <rect x="2.5" y="0.5" width="5" height="5" rx="1.5" stroke="currentColor" fill="none" />
      </svg>
    </span>
  );
}

/**
 * The chip form: all-day bars and month-grid rows.
 *
 * 22px tall, 6px radius, 12/20 type — Google's all-day measurements. It spans
 * days rather than repeating once per day, so a five-day trip reads as one bar.
 */
export function EventChip({
  event,
  hue,
  dark,
  tone,
  past,
  selected,
  dimmed = false,
  copies = 1,
  showTime = false,
  style,
  onSelect,
  blockRef,
}: {
  event: CalendarEvent;
  hue: HueIndex;
  dark: boolean;
  tone: EventTone;
  past: boolean;
  selected: boolean;
  /** Ruled out by a filter — faded, not hidden, so the month keeps its shape. */
  dimmed?: boolean;
  copies?: number;
  showTime?: boolean;
  style?: CSSProperties;
  onSelect: () => void;
  blockRef?: (node: HTMLButtonElement | null) => void;
}) {
  const paint = paintFor(hue, tone, { dark, past });

  return (
    <button
      type="button"
      ref={blockRef}
      tabIndex={-1}
      data-selected={selected || undefined}
      onClick={onSelect}
      title={event.title}
      className={cn(
        "relative flex items-center gap-1 overflow-hidden text-left",
        selected && "ring-2 ring-accent ring-offset-1 ring-offset-background",
      )}
      style={{
        borderRadius: BLOCK_RADIUS,
        background: paint.background,
        color: paint.color,
        opacity: dimmed ? 0.3 : paint.opacity,
        boxShadow: paint.border ? `inset 0 0 0 1px ${paint.border}` : undefined,
        padding: "0 8px",
        fontSize: 12,
        lineHeight: "20px",
        fontWeight: 500,
        ...style,
      }}
    >
      {showTime && !event.allDay && (
        <span className="shrink-0 tabular-nums" style={{ color: paint.timeColor, opacity: 0.85 }}>
          {shortTime(event.start)}
        </span>
      )}
      <span className={cn("min-w-0 flex-1 truncate", paint.strikethrough && "line-through")}>
        {event.title}
      </span>
      {copies > 1 && <MergeMark count={copies} />}
    </button>
  );
}
