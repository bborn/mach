import { useCallback, useMemo, useRef, useState, type MouseEvent, type ReactNode } from "react";
import type { Thread, ThreadId } from "@/types";
import { useKeyBindings, useKeymap } from "@/hooks/useKeymap";
import { overlayOwnsKeyboard, useMach } from "@/hooks/useMach";
import type { KeyBinding } from "@/lib/keymap";
import { keyEventFromToken } from "@/lib/menu";
import { selectOnly, type Selection } from "@/lib/selection";
import { openSearch } from "@/components/search/palette";
import {
  ContextMenu,
  ContextMenuItem,
  ContextMenuLabel,
  ContextMenuSeparator,
} from "@/components/ui/context-menu";

/**
 * Right-click on a conversation.
 *
 * # It is a second surface, not a second implementation
 *
 * Every item here is a binding lifted straight out of the keymap registry, and
 * choosing one calls **that binding's own handler**. Not a copy of it, not a
 * command assembled next to it — the same function `e` calls. Undo, the toast,
 * the optimistic row removal and the partial-failure reselect are therefore not
 * features of this menu at all; they are features of the thing it delegates to,
 * and they cannot drift out of step with it.
 *
 * The consequence worth stating: **an item whose binding is not live does not
 * appear.** The menu is built from a snapshot of `keymap.active()` taken at the
 * moment of the right-click, so it can only ever offer what the keyboard could
 * have done from that same position. There is no list of labels here that could
 * one day name something the app no longer does.
 *
 * That is also why several obvious entries are missing. Mark as unread, and
 * applying a label, are real commands in `lib/data.ts` — and neither has a
 * binding or an action in `useMach`, so there is no existing path to route
 * through and inventing one would be exactly the second implementation this
 * comment is about. Spark's Pin maps to Mach's ⇧F, which favourites *the open
 * conversation* rather than the row under the pointer; on the wrong row it
 * would quietly pin something else, so it is out too.
 *
 * # Which conversations it acts on
 *
 * The keyboard's rule is `commandTargets`: the selection if there is one,
 * otherwise the row under the cursor. A pointer adds a third possibility — a
 * row that is neither — and the answer is Finder's:
 *
 *   * right-click **inside** a selection — the selection is the target, and the
 *     menu says how many.
 *   * right-click **outside** one, or with nothing selected — that row becomes
 *     the selection, so `commandTargets` resolves to it.
 *
 * Making it the *selection* rather than moving the cursor to it is deliberate.
 * The cursor is what the reading pane follows, and following it marks the
 * conversation read on Google — a right-click must not do that. A selection of
 * one is a statement about what the next command acts on and nothing else.
 * Dismissing the menu without choosing anything puts the previous selection
 * back.
 *
 * # Keyboard
 *
 * ⇧F10 and the Menu key open it on the cursor row, anchored to the row rather
 * than to a pointer that was never there. Arrows, Enter, typeahead and Escape
 * are Base UI's, which they can only be because `ContextMenu` claims the
 * keyboard away from the registry while it is up — see `components/ui/context-menu.tsx`.
 */

/** Where the popup hangs: a row element, or a point the pointer was at. */
type Anchor = Element | { getBoundingClientRect: () => DOMRect };

interface OpenMenu {
  anchor: Anchor;
  /**
   * The menu, resolved once at the moment it was asked for.
   *
   * Held rather than recomputed per render because it is a *snapshot*: the
   * bindings it was built from were live then, and the claim it is about to
   * take means asking the registry again would answer "almost nothing".
   */
  items: Item[];
  count: number;
  /** The selection to put back if the menu is dismissed without a choice. */
  restore: Selection | null;
}

export function ThreadContextMenu({ children }: { children: ReactNode }) {
  const { ui, dispatch, visibleThreads } = useMach();
  const keymap = useKeymap();
  const [menu, setMenu] = useState<OpenMenu | null>(null);
  const chosen = useRef(false);
  const returnTo = useRef<HTMLElement | null>(null);

  const byId = useMemo(
    () => new Map(visibleThreads.map((thread) => [thread.id, thread])),
    [visibleThreads],
  );

  /**
   * Open on `id`, having first made sure `commandTargets` will resolve to what
   * the user pointed at.
   *
   * The snapshot of the registry is taken *here*, before the menu mounts and
   * claims the keyboard — ask afterwards and every mail binding has already
   * been silenced by the claim, which is the same trap `ShortcutSheet`
   * documents.
   */
  const open = useCallback(
    (id: ThreadId, anchor: Anchor) => {
      if (!byId.has(id)) return;
      const selected = ui.selection.ids.includes(id);
      const targets = selected ? [...ui.selection.ids] : [id];
      const items = buildItems(keymap.active(), targets, ui.threadId, byId);
      // An empty menu is not a menu. Bail before anything is dispatched, so a
      // right-click that has nothing to offer also has no side effect.
      if (items.length === 0) return;

      // Nothing to retarget when the row is already the whole story: it is
      // either in the selection, or the cursor row with no selection around it.
      let restore: Selection | null = null;
      if (!selected && !(ui.selection.ids.length === 0 && ui.threadId === id)) {
        restore = ui.selection;
        dispatch({
          type: "selection",
          selection: selectOnly(ui.selection, [id], visibleThreads.map((t) => t.id)),
        });
      }

      // Touching the list is a claim on the keyboard, wherever it was — the
      // same line `clickThread` opens with, for the same reason.
      if (ui.focus !== "list") dispatch({ type: "focus", focus: "list" });

      returnTo.current = document.activeElement as HTMLElement | null;
      chosen.current = false;
      setMenu({ anchor, items, count: targets.length, restore });
    },
    [byId, dispatch, keymap, ui.focus, ui.selection, ui.threadId, visibleThreads],
  );

  const onContextMenu = useCallback(
    (event: MouseEvent) => {
      const row = (event.target as Element | null)?.closest?.("[data-thread-id]");
      const id = Number(row?.getAttribute("data-thread-id"));
      if (!row || !Number.isFinite(id)) return;
      event.preventDefault();
      const { clientX, clientY } = event;
      open(id, { getBoundingClientRect: () => new DOMRect(clientX, clientY, 0, 0) });
    },
    [open],
  );

  /* ------------------------------------------------------------- keyboard --- */

  const canOpenFromKeyboard = () =>
    ui.mode === "mail" &&
    ui.focus === "list" &&
    ui.threadId !== null &&
    menu === null &&
    !overlayOwnsKeyboard(ui);

  const openAtCursor = () => {
    if (ui.threadId === null) return;
    const row = document.querySelector(`[data-thread-id="${ui.threadId}"]`);
    if (!row) return;
    open(ui.threadId, row);
  };

  useKeyBindings([
    {
      keys: "shift+f10",
      group: "Mail",
      description: "Menu for the conversation",
      when: canOpenFromKeyboard,
      handler: openAtCursor,
    },
    {
      // The dedicated Menu key, where a keyboard has one. Undocumented in the
      // sheet because `formatBinding` has no glyph for it and "CONTEXTMENU" in
      // the key column would be worse than the row's absence.
      keys: "contextmenu",
      when: canOpenFromKeyboard,
      handler: openAtCursor,
    },
  ]);

  /* ------------------------------------------------------------------ menu --- */

  const close = useCallback(
    (next: boolean) => {
      if (next) return;
      // Dismissed rather than used: the one-row selection the menu made to aim
      // itself was never something the user asked for, so it goes away with it.
      if (menu?.restore && !chosen.current) {
        dispatch({ type: "selection", selection: menu.restore });
      }
      setMenu(null);
    },
    [dispatch, menu],
  );

  const items = menu?.items ?? [];
  const count = menu?.count ?? 0;

  return (
    <>
      <div onContextMenu={onContextMenu}>{children}</div>
      <ContextMenu
        open={menu !== null}
        onOpenChange={close}
        anchor={menu?.anchor ?? null}
        finalFocus={returnTo}
        label={count > 1 ? `${count} conversations` : "Conversation"}
      >
        {count > 1 && <ContextMenuLabel>{count} conversations</ContextMenuLabel>}
        {items.map((item) =>
          item.kind === "separator" ? (
            <ContextMenuSeparator key={item.key} />
          ) : (
            <ContextMenuItem
              key={item.key}
              shortcut={item.shortcut}
              tone={item.tone}
              onClick={() => {
                chosen.current = true;
                item.run();
              }}
            >
              {item.label}
            </ContextMenuItem>
          ),
        )}
      </ContextMenu>
    </>
  );
}

/* -------------------------------------------------------------------------- */
/* Turning the registry into a menu                                            */
/* -------------------------------------------------------------------------- */

export type Item =
  | { kind: "separator"; key: string }
  | {
      kind: "item";
      key: string;
      label: string;
      shortcut?: string;
      tone?: "default" | "danger";
      run: () => void;
    };

/**
 * The one place a group-and-description pair is written down.
 *
 * It is a lookup into the registry, not a description of an action: if the pair
 * stops matching a live binding — renamed, regrouped, deleted — the item simply
 * does not appear, which is the failure mode worth having. The alternative is a
 * menu that offers something and does nothing.
 */
function find(
  bindings: readonly KeyBinding[],
  group: string,
  description: string,
): KeyBinding | undefined {
  return bindings.find((b) => b.group === group && b.description === description);
}

export function buildItems(
  bindings: readonly KeyBinding[],
  targets: readonly ThreadId[],
  /** The open conversation — what the composer's keys answer, if anything. */
  cursor: ThreadId | null,
  byId: Map<ThreadId, Thread>,
): Item[] {
  const threads = targets.map((id) => byId.get(id)).filter((t): t is Thread => t !== undefined);
  const single = threads.length === 1 ? threads[0] : undefined;
  const items: Item[] = [];

  const push = (
    group: string,
    description: string,
    label: string,
    tone?: "default" | "danger",
  ) => {
    const binding = find(bindings, group, description);
    if (!binding) return;
    items.push({
      kind: "item",
      key: `${group}/${description}`,
      label,
      shortcut: binding.keys,
      tone,
      // The binding's own handler, so this is the keystroke by another route.
      // `keyEventFromToken` hands it the event the key would have produced;
      // none of these read it, and one day one might.
      run: () => void binding.handler(keyEventFromToken(binding.keys.split(/\s+/)[0] ?? "")),
    });
  };

  /**
   * The put-back, whatever this mailbox calls it.
   *
   * The one item found by its key rather than by its description, because the
   * description is the thing that varies: ⇧E is registered once with the name
   * of the command that is live — "Restore", "Not spam", "Move to inbox" — so
   * the menu can only say the right word by reading it off the binding. Same
   * guarantee either way round: no live binding, no item.
   */
  const pushPutBack = () => {
    const binding = bindings.find((b) => b.group === "Actions" && b.keys === "shift+e");
    if (!binding?.description) return;
    items.push({
      kind: "item",
      key: "Actions/putBack",
      label: binding.description,
      shortcut: binding.keys,
      run: () => void binding.handler(keyEventFromToken("shift+e")),
    });
  };

  const separate = () => {
    if (items.length > 0 && items[items.length - 1]?.kind !== "separator") {
      items.push({ kind: "separator", key: `sep-${items.length}` });
    }
  };

  /*
   * Writing is offered only when the row under the pointer *is* the open
   * conversation. Reply, Reply all and Forward are all `ComposerDock`'s, and
   * what they answer is `ui.threadId` and the detail loaded for it — not
   * whatever the pointer happens to be over. On any other row the item would
   * quietly reply to something else, and on a multi-selection it means nothing
   * at all.
   */
  if (single && single.id === cursor) {
    push("Write", "Reply", "Reply");
    push("Write", "Reply all", "Reply all");
    push("Write", "Forward", "Forward");
  }

  separate();
  // Matches `starSelected`: a mixed set gets starred, only an all-starred set
  // is unstarred. The label has to say which, or it is a coin toss.
  const allStarred = threads.length > 0 && threads.every((t) => t.starred);
  push("Actions", "Star", allStarred ? "Unstar" : "Star");
  push("Actions", "Snooze", "Snooze");
  // Both keys are always registered together; which one is worth offering is
  // the same question `SelectionBar` asks — see `readAction` there.
  const anyUnread = threads.some((t) => t.unread);
  if (anyUnread) push("Actions", "Mark read", "Mark read");
  else if (threads.length > 0) push("Actions", "Mark unread", "Mark unread");

  separate();
  push("Actions", "Archive", "Archive");
  pushPutBack();
  push("Actions", "Trash", "Trash", "danger");
  // Drafts, where `#` means this instead. It asks before it acts, exactly as
  // the key does — choosing it puts the question in the selection bar.
  push("Actions", "Discard drafts", "Discard", "danger");

  /*
   * Not a `Command` and deliberately without a key: `openSearch` is the same
   * seam ⌘K's operator layer hands a query over through, and `from:` is a real
   * operator of the real parser. It acts on the sender of one conversation, so
   * it is offered for one.
   */
  const sender = single?.participants[0]?.email;
  if (sender) {
    separate();
    items.push({
      kind: "item",
      key: "search/from",
      label: "Search by sender",
      run: () => openSearch(`from:${sender}`),
    });
  }

  // A leading or trailing rule is a rule around nothing.
  while (items[0]?.kind === "separator") items.shift();
  while (items[items.length - 1]?.kind === "separator") items.pop();
  return items;
}
