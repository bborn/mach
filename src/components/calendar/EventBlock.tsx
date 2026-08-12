import type { CSSProperties, PointerEvent as ReactPointerEvent } from "react";
import type { CalendarEvent } from "@/types";
import type { ResizeEdge } from "@/lib/calendar-drag";
import {
  BLOCK_RADIUS,
  blockPlan,
  type BlockPlan,
} from "@/lib/calendar-geometry";
import {
  paintFor,
  type BlockPaint,
  type CalendarColor,
  type EventTone,
} from "@/lib/calendar-palette";
import { shortTime } from "@/lib/time";
import { cn } from "@/lib/utils";

/* -------------------------------------------------------------------------- */
/* The selection cursor                                                        */
/* -------------------------------------------------------------------------- */

/**
 * What "this is the block you are on" looks like.
 *
 * The old mark was `ring-2 ring-accent ring-offset-1`, and it failed for three
 * separate reasons at once:
 *
 *   1. **It was a hue against hues.** The accent is a blue at L≈0.55; the
 *      calendar ramp puts a fill at L=0.54 on every hue including 250°, which
 *      is the accent's own neighbourhood. A blue ring on a blue calendar is not
 *      a ring. Colour cannot be the signal when the thing behind it is
 *      user-chosen colour.
 *   2. **On half the blocks it was not drawn at all.** Tailwind's `ring-*`
 *      utilities compile to `box-shadow`, and every outlined block — an
 *      unanswered invitation, a tentative one, a declined one — sets
 *      `style.boxShadow` inline for its 1px border. An inline property beats a
 *      class rule outright, so selecting an unanswered invite silently painted
 *      no cursor whatsoever.
 *   3. **It faded with the block.** Dragging sets `opacity: 0.35` on the
 *      button, and `opacity` takes the ring with it — the cursor dimmed out at
 *      exactly the moment you were moving something and most needed it.
 *
 * The replacement is a *luminance sandwich*, not a colour: outward from the
 * block edge, 2px of a gap colour, then 3px of accent. The inner band is the
 * load-bearing one, and what it is made of has had to change.
 *
 * It used to be `var(--background)` unconditionally, and that worked because
 * the calendar ramp held every fill at one middling lightness (L 0.54 light,
 * L 0.50 dark), so the page — near-white or near-black — could never approach a
 * fill. Fills are now the user's own colours and span the whole range: a white
 * gap against `#fbd75b` on a white page is a 1.4:1 step, which is not a gap,
 * and the mark would quietly fail on exactly the pale calendars where it is
 * hardest to see anyway.
 *
 * So the gap is the block's **own ink** — the black or white the text is drawn
 * in, which `paintFor` already chose to clear 4.5:1 against the fill. The step
 * between fill and gap is therefore guaranteed rather than assumed, on every
 * colour, in both themes.
 *
 * Note what this does *not* change. In light mode the ink is white exactly when
 * the fill is dark, which is when `var(--background)` was already white; in
 * dark mode the ink is black exactly when the fill is light, which is when
 * `var(--background)` was already near-black. The new rule agrees with the old
 * one in every case where the old one worked, and differs only where it broke.
 *
 * Beyond the gap, the accent band still steps against the page, and the outer
 * two steps are unchanged. Desaturate the whole thing and the mark survives,
 * which is the test that matters for a colour-blind reader.
 *
 * The structural half is the drop shadow plus a raised z-index: the selected
 * block lifts off the grid and casts over its neighbours instead of being
 * overlapped by them. That is a cue with no hue in it at all.
 *
 * Drawn entirely in `box-shadow` — never `outline`, never `border`, never a
 * changed width — so it costs the block no geometry and arrow-stepping through
 * a day cannot make the grid twitch.
 */
export function selectionShadow(gap: string): string {
  return [
    `0 0 0 2px ${gap}`,
    "0 0 0 5px var(--accent)",
    "0 3px 10px -2px color-mix(in oklab, var(--foreground) 45%, transparent)",
  ].join(", ");
}

/**
 * The same mark, one pixel tighter, for chips.
 *
 * All-day bars and month cells sit inside padded containers that clip; 6px of
 * halo on a 22px chip in a 4px-padded month cell loses its outer band to
 * `overflow-hidden`. Five reads identically and stays inside the box.
 */
function selectionShadowChip(gap: string): string {
  return [
    `0 0 0 2px ${gap}`,
    "0 0 0 4px var(--accent)",
    "0 2px 8px -2px color-mix(in oklab, var(--foreground) 45%, transparent)",
  ].join(", ");
}

/**
 * Layer the block's own border (if it has one) under the selection mark.
 *
 * `gap` comes from the *unfaded* paint on purpose: a block mid-drag is washed
 * towards the page, and washing the cursor's inner band with it would undo
 * defect 3 above by another route.
 */
function shadowFor(
  border: string | undefined,
  selected: boolean,
  chip: boolean,
  gap: string,
  /**
   * A cascaded block's left edge, one pixel wide.
   *
   * In a cluster that cascades, blocks overlap rather than sitting in their own
   * columns, so the 1px gutter that separates two divided blocks is not there.
   * Two events on the same calendar would otherwise merge into one long
   * rectangle with a rounded notch in it.
   *
   * The hairline is the block's own ink, which is what the selection cursor's
   * inner band uses and for the same reason: `paintFor` chose it to clear 4.5:1
   * against this fill, so the separation is guaranteed. It is suppressed while
   * selected, because the cursor's gap band starts 2px out and a hairline drawn
   * over it would nick the mark.
   */
  cascadeEdge?: string,
): string | undefined {
  const layers = [
    border,
    selected ? undefined : cascadeEdge,
    selected ? (chip ? selectionShadowChip(gap) : selectionShadow(gap)) : undefined,
  ];
  const drawn = layers.filter((layer): layer is string => layer !== undefined);
  return drawn.length > 0 ? drawn.join(", ") : undefined;
}

/**
 * Dimming that leaves the cursor alone.
 *
 * `opacity` on the whole button was the obvious way to fade a block that is
 * being dragged or that a type-to-select filter has ruled out, and it faded the
 * selection ring with it. Wash the *paint* towards the page instead: the fill
 * and the text recede exactly as before, while the mark is drawn from the
 * unfaded paint and stays at full strength.
 */
function faded(paint: BlockPaint): BlockPaint {
  const wash = (color: string, keep: number) =>
    `color-mix(in oklab, ${color} ${keep}%, var(--background))`;
  return {
    ...paint,
    background: wash(paint.background, 30),
    color: wash(paint.color, 45),
    border: paint.border === undefined ? undefined : wash(paint.border, 35),
    timeColor: wash(paint.timeColor, 45),
  };
}

export interface EventBlockProps {
  event: CalendarEvent;
  /** The calendar's colour — Google's hex, or the hashed fallback. */
  color: CalendarColor;
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
  /**
   * This block is cascaded over the one to its left, so it draws its own edge.
   * Only ever true for a block whose cluster stopped dividing — see
   * `clusterPlan` in `calendar-geometry.ts`.
   */
  cascaded?: boolean;
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
  color,
  dark,
  tone,
  past,
  selected,
  copies,
  height,
  width,
  cascaded = false,
  dimmed = false,
  resizable = false,
  style,
  onSelect,
  onGrab,
  blockRef,
  onPointerEnter,
  onPointerLeave,
}: EventBlockProps) {
  const plan = blockPlan(height, { hasLocation: Boolean(event.location), width });
  const painted = paintFor(color, tone, { dark, past });
  const paint = dimmed ? faded(painted) : painted;
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
      // The keyboard cursor, said out loud. A screen reader stepping the week
      // with the arrow keys otherwise has no way to know which block it is on,
      // because nothing here ever takes DOM focus.
      aria-current={selected ? "true" : undefined}
      // `tabIndex={-1}` keeps blocks out of the browser's own tab order: Tab
      // steps event-to-event through the keymap, in start order, which is the
      // order the week reads in — not the order the DOM happens to be in.
      tabIndex={-1}
      data-selected={selected || undefined}
      // Which event this rectangle is, for anything that has to work back from
      // a pointer or from `ui.eventId` to a DOM node — the right-click menu is
      // the only such thing today, and ⇧F10 anchors its popup to this element.
      data-event-id={event.id}
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
        "absolute text-left transition-[left,width,box-shadow,background-color] duration-[120ms] ease-out motion-reduce:transition-none",
        plan.overflow ? "overflow-visible" : "overflow-hidden",
        onGrab && "cursor-grab active:cursor-grabbing",
      )}
      style={{
        borderRadius: BLOCK_RADIUS,
        background: paint.background,
        color: paint.color,
        // The dragged block stays put and fades: the ghost is the thing under
        // the pointer, and the hole it left is useful context for where it was.
        // The fade lives in `faded()` now, in the paint rather than in
        // `opacity`, so it cannot take the selection mark down with it.
        opacity: paint.opacity,
        // Selection is a halo on the outside; `:focus-visible` in globals.css
        // draws its outline on the inside. Two different marks, so "the cursor
        // is here" and "the browser focus is here" never read as one thing.
        boxShadow: shadowFor(
          ring,
          selected,
          false,
          painted.selectionGap,
          cascaded ? `-1px 0 0 0 ${painted.selectionGap}` : undefined,
        ),
        // A dashed border cannot be faked with an inset shadow; tentative
        // events get a real one, inset so it does not change the geometry.
        // A sliver has no room for one either — see `sliverOutline` above.
        outline:
          paint.borderStyle === "dashed" && !sliverOutline
            ? `1px dashed ${paint.border}`
            : undefined,
        outlineOffset: paint.borderStyle === "dashed" && !sliverOutline ? -1 : undefined,
        // 2px is the documented exception to the 4pt grid (see globals.css): a
        // 30-minute block is 24px tall and holds one 15px line, so its vertical
        // inset is 2 or it is zero. The horizontal inset is on the grid at 4,
        // which also returns four pixels to the title in a crowded column.
        padding: plan.tier === "full" || plan.tier === "twoLine" ? "2px 4px" : 0,
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
      className="pointer-events-none absolute inset-x-0 flex items-center gap-1 overflow-hidden px-1"
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
        className={cn("min-w-0 shrink overflow-hidden font-semibold", strike && "line-through")}
        style={{ textOverflow: "ellipsis", color: titleColor }}
      >
        {plan.showTime ? `${title},` : title}
      </span>
      {plan.showTime && (
        <span
          className="shrink-0 tabular-nums font-normal"
          style={{ color: paint, fontSize: plan.timeFontPx }}
        >
          {time}
        </span>
      )}
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
  // the time is what disambiguates two blocks with similar names — right up
  // until the block is too narrow for both, at which point the title wins and
  // the grid position is left to say when.
  if (plan.inlineTime) {
    return (
      <span className="flex items-center gap-1 px-1" style={lineStyle}>
        <span className={cn("min-w-0 shrink truncate font-semibold", strike && "line-through")}>
          {plan.showTime ? `${event.title},` : event.title}
        </span>
        {plan.showTime && (
          <span
            className="shrink-0 tabular-nums font-normal"
            style={{ color: timeColor, fontSize: plan.timeFontPx }}
          >
            {time}
          </span>
        )}
      </span>
    );
  }

  const secondary: CSSProperties = {
    color: timeColor,
    opacity: 0.85,
    fontSize: plan.timeFontPx,
    fontWeight: 400,
  };

  return (
    <span className="block" style={lineStyle}>
      <span
        className={cn(
          "block font-semibold",
          plan.titleLines === 3
            ? "line-clamp-3"
            : plan.titleLines === 2
              ? "line-clamp-2"
              : "truncate",
          strike && "line-through",
        )}
      >
        {event.title}
      </span>
      {plan.showTime && (
        <span className="block truncate tabular-nums" style={secondary}>
          {time}
        </span>
      )}
      {plan.showLocation && (
        <span className="block truncate" style={secondary}>
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
      className="pointer-events-none absolute right-1 top-1 opacity-80"
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
  color,
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
  color: CalendarColor;
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
  const painted = paintFor(color, tone, { dark, past });
  const paint = dimmed ? faded(painted) : painted;

  return (
    <button
      type="button"
      ref={blockRef}
      tabIndex={-1}
      data-selected={selected || undefined}
      data-event-id={event.id}
      aria-current={selected ? "true" : undefined}
      onClick={onSelect}
      title={event.title}
      className={cn(
        "relative flex items-center gap-1 overflow-hidden text-left",
        "transition-[box-shadow,background-color] duration-[120ms] ease-out motion-reduce:transition-none",
      )}
      style={{
        borderRadius: BLOCK_RADIUS,
        background: paint.background,
        color: paint.color,
        opacity: paint.opacity,
        boxShadow: shadowFor(
          paint.border ? `inset 0 0 0 1px ${paint.border}` : undefined,
          selected,
          true,
          painted.selectionGap,
        ),
        // A chip stacked against its neighbours has to cast the halo over them
        // rather than under, or the row above eats the top band of the mark.
        zIndex: selected ? 1 : undefined,
        padding: "0 8px",
        fontSize: 12,
        lineHeight: "20px",
        // 600, matching the timed block's title. A chip is all title — there is
        // no second line for the weight to be contrasted against — so it takes
        // the same weight the title takes everywhere else on the grid.
        fontWeight: 600,
        ...style,
      }}
    >
      {showTime && !event.allDay && (
        <span
          className="shrink-0 font-normal tabular-nums"
          style={{ color: paint.timeColor, opacity: 0.85, fontSize: 11 }}
        >
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
