import { Menu } from "@base-ui/react/menu";
import { useLayoutEffect, type ComponentProps, type ReactNode, type RefObject } from "react";
import { useKeymap } from "@/hooks/useKeymap";
import { formatBinding } from "@/lib/keymap";
import { cn } from "@/lib/utils";
import { POPUP_MARKER, POPUP_SURFACE, usePopupRegistration } from "./popup";

/**
 * A right-click menu on the app's one floating surface.
 *
 * Built on Base UI's `Menu` rather than its `ContextMenu`, which looks like the
 * obvious choice and is not. `ContextMenu.Trigger` owns both the anchor and the
 * gesture: it opens where the pointer was, and only where the pointer was. A
 * menu that can only be opened by a mouse fails this app's first rule, so the
 * anchor has to be something a caller can supply — the pointer for a
 * right-click, the row itself for ⇧F10 — and that is what `anchor` is for.
 *
 * # Why it claims the keyboard
 *
 * The keymap owns one `keydown` listener in the capture phase, so the list's
 * `j`, `k`, Enter and arrows are decided before Base UI ever sees the event.
 * Without a claim, arrowing through this menu would walk the conversation list
 * behind it and Enter would open a thread. `claimKeyboard` silences everything
 * below the overlay floor for as long as the menu is up, which leaves the keys
 * to reach the DOM untouched — the same mechanism `Overlay` uses, for the same
 * reason. See `lib/keymap.ts`.
 *
 * # The shortcut column
 *
 * Items take a binding string, not a rendered glyph, and `formatBinding` draws
 * it. Nothing here may hardcode "⌘E": a menu that spells its own shortcuts is a
 * second copy of the keymap, and it starts lying the day somebody rebinds a
 * key. `src-tauri/src/shell.rs` builds the macOS menu bar on the same rule.
 */

export interface ContextMenuProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /**
   * What the popup hangs off. A `VirtualElement` — anything with
   * `getBoundingClientRect` — is how a pointer position becomes an anchor.
   */
  anchor: Menu.Positioner.Props["anchor"];
  /** Where focus goes when the menu closes. */
  finalFocus?: RefObject<HTMLElement | null>;
  /** What the menu acts on, for anyone listening rather than looking. */
  label?: string;
  children: ReactNode;
}

export function ContextMenu({
  open,
  onOpenChange,
  anchor,
  finalFocus,
  label,
  children,
}: ContextMenuProps) {
  usePopupRegistration(open);
  const keymap = useKeymap();

  /*
   * Layout effect, not effect — the same reasoning as `Overlay`: the claim has
   * to be in force before the browser paints, or there is a frame in which the
   * menu is on screen and `e` still archives the conversation behind it.
   */
  useLayoutEffect(() => {
    if (!open) return;
    return keymap.claimKeyboard();
  }, [open, keymap]);

  return (
    <Menu.Root open={open} onOpenChange={(next) => onOpenChange(next)}>
      <Menu.Portal>
        <Menu.Positioner
          anchor={anchor}
          side="bottom"
          align="start"
          sideOffset={2}
          // Enough that the popup never touches a window edge, which is where
          // the shadow stops reading as a shadow.
          collisionPadding={8}
          className="isolate z-50 outline-none"
          {...POPUP_MARKER}
        >
          <Menu.Popup
            data-slot="context-menu"
            aria-label={label}
            finalFocus={finalFocus}
            className={cn(POPUP_SURFACE, "min-w-[13rem] py-1 text-body text-foreground")}
          >
            {children}
          </Menu.Popup>
        </Menu.Positioner>
      </Menu.Portal>
    </Menu.Root>
  );
}

export interface ContextMenuItemProps extends Menu.Item.Props {
  /** A keymap binding string — `"e"`, `"shift+f"`. Rendered, never typed out. */
  shortcut?: string;
  tone?: "default" | "danger";
}

export function ContextMenuItem({
  className,
  children,
  shortcut,
  tone = "default",
  ...props
}: ContextMenuItemProps) {
  return (
    <Menu.Item
      data-slot="context-menu-item"
      className={cn(
        "group flex min-h-7 cursor-default items-center gap-6 px-2.5 py-1",
        "text-body outline-none select-none",
        // The row-hover step, not the accent: `SelectItem` made this call
        // first, and a menu that paints a saturated bar under the pointer
        // feels like a website. The two surfaces have to agree.
        tone === "danger"
          ? "text-danger data-highlighted:bg-row-hover"
          : "text-foreground data-highlighted:bg-row-hover",
        "data-disabled:pointer-events-none data-disabled:opacity-40",
        className,
      )}
      {...props}
    >
      <span className="min-w-0 flex-1 truncate">{children}</span>
      {shortcut && (
        <span className="shrink-0 font-mono text-micro text-faint-foreground">
          {formatBinding(shortcut)}
        </span>
      )}
    </Menu.Item>
  );
}

/**
 * What the menu is about to act on, when that is more than one thing.
 *
 * A plain div rather than `Menu.GroupLabel`, which has to sit inside a
 * `Menu.Group` to label anything and would then be claiming that the items
 * below it are a group, which they are not — this heads the whole menu. The
 * screen-reader version of the same fact is `ContextMenu`'s `label`, so this
 * one is hidden rather than read out twice.
 */
export function ContextMenuLabel({
  className,
  ...props
}: ComponentProps<"div">) {
  return (
    <div
      data-slot="context-menu-label"
      aria-hidden
      className={cn(
        "px-2.5 pt-0.5 pb-1 text-micro text-faint-foreground select-none",
        className,
      )}
      {...props}
    />
  );
}

export function ContextMenuSeparator({ className, ...props }: Menu.Separator.Props) {
  return (
    <Menu.Separator
      data-slot="context-menu-separator"
      className={cn("pointer-events-none my-1 h-px bg-border", className)}
      {...props}
    />
  );
}
