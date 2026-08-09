import { useEffect, useRef, type RefObject } from "react";
import { IDLE_GESTURE, feedWheel, type WheelGesture } from "@/lib/calendar-gesture";

/**
 * The DOM half of trackpad navigation. The gesture arithmetic lives in
 * `calendar-gesture.ts`; this is the part that has to know about the page —
 * which events belong to something else, and when to take one over.
 *
 * The listener is deliberately not React's `onWheel`. React attaches wheel
 * handlers passively at the root, so `preventDefault` from one is a silent
 * no-op, and a horizontal swipe that reaches the webview unclaimed is a
 * back-navigation out of the app.
 */

/**
 * Anything on the calendar surface that owns the pointer while it exists: the
 * ghost of a block being dragged or resized, and a drag-to-create draft, which
 * stays on screen after the button comes up while its title is typed.
 */
const BUSY = "[data-calendar-drag], [data-draft]";

interface PeriodWheelOptions {
  /** The element gestures are read from — the grid, not the whole window. */
  ref: RefObject<HTMLElement | null>;
  /** True in month view, where a vertical swipe also moves a period. */
  vertical: boolean;
  /** False whenever something else owns the surface: a modal, the palette. */
  enabled: boolean;
  onStep: (delta: -1 | 1) => void;
}

export function usePeriodWheel({ ref, vertical, enabled, onStep }: PeriodWheelOptions): void {
  const gesture = useRef<WheelGesture>(IDLE_GESTURE);

  // The handler reads its inputs through a ref rather than closing over them,
  // so the listener survives every re-render. Re-attaching between two events
  // of one stream would be fine, but resetting the gesture with it would not:
  // a period change re-renders the grid mid-flick, and the tail has to land on
  // the same gesture that fired, or it fires again.
  const live = useRef({ vertical, enabled, onStep });
  useEffect(() => {
    live.current = { vertical, enabled, onStep };
  });

  useEffect(() => {
    const node = ref.current;
    if (!node) return;

    function onWheel(event: WheelEvent) {
      const { vertical: pagesVertically, enabled: on, onStep: step } = live.current;
      const host = event.currentTarget;
      if (!on || !(host instanceof HTMLElement)) {
        gesture.current = IDLE_GESTURE;
        return;
      }

      // A held button means a drag is in flight. Scrolling to reach an hour
      // that is off screen is a normal thing to do mid-drag, so the wheel keeps
      // working — it just stops meaning "next week". `buttons` is the reliable
      // signal for a press; the drag ghost and the draft cover the moment after
      // the button comes up but before the drag has resolved.
      if (event.buttons !== 0 || host.querySelector(BUSY) !== null) {
        gesture.current = IDLE_GESTURE;
        return;
      }

      // The one thing on the month grid with somewhere of its own to scroll is
      // an expanded day cell. Its scrolling wins; the month only moves once
      // there is nothing under the pointer left to move.
      if (
        pagesVertically &&
        Math.abs(event.deltaY) > Math.abs(event.deltaX) &&
        scrollsAlready(event.target, host, event.deltaY)
      ) {
        gesture.current = IDLE_GESTURE;
        return;
      }

      const outcome = feedWheel(
        gesture.current,
        {
          deltaX: event.deltaX,
          deltaY: event.deltaY,
          deltaMode: event.deltaMode,
          timeStamp: event.timeStamp,
          ctrlKey: event.ctrlKey,
        },
        { vertical: pagesVertically },
      );
      gesture.current = outcome.gesture;

      if (outcome.claimed) event.preventDefault();
      if (outcome.step !== 0) step(outcome.step);
    }

    node.addEventListener("wheel", onWheel, { passive: false });
    return () => node.removeEventListener("wheel", onWheel);
  }, [ref]);
}

/** Whether anything between the pointer and the grid can still scroll this way. */
function scrollsAlready(target: EventTarget | null, host: HTMLElement, deltaY: number): boolean {
  let node = target instanceof HTMLElement ? target : null;
  while (node !== null && node !== host.parentElement) {
    const overflow = getComputedStyle(node).overflowY;
    if (overflow === "auto" || overflow === "scroll") {
      const room =
        deltaY > 0
          ? node.scrollHeight - node.clientHeight - node.scrollTop
          : node.scrollTop;
      // A pixel of slack: a scroller sitting exactly at its end reports a
      // fractional remainder on a fractional-DPI display.
      if (room > 1) return true;
    }
    node = node.parentElement;
  }
  return false;
}
