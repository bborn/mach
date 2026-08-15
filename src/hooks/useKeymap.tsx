import {
  createContext,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { createKeymap, type KeyBinding, type Keymap } from "@/lib/keymap";
import { connectMenu } from "@/lib/menu";

const KeymapContext = createContext<Keymap | null>(null);

/**
 * Mounts the one and only keydown listener. Capture phase, so a binding wins
 * over anything a component might do with the same key, and so Escape still
 * works while focus is inside the palette input.
 */
export function KeymapProvider({ children }: { children: ReactNode }) {
  const keymap = useMemo(() => createKeymap(), []);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      keymap.handle({
        key: event.key,
        metaKey: event.metaKey,
        ctrlKey: event.ctrlKey,
        altKey: event.altKey,
        shiftKey: event.shiftKey,
        code: event.code,
        repeat: event.repeat,
        target: event.target as unknown as { tagName?: string; isContentEditable?: boolean },
        preventDefault: () => event.preventDefault(),
        stopPropagation: () => event.stopPropagation(),
      });
    };
    window.addEventListener("keydown", onKeyDown, true);
    return () => window.removeEventListener("keydown", onKeyDown, true);
  }, [keymap]);

  /*
   * The macOS menu bar feeds this same registry rather than calling into the
   * app separately, so a menu item and its shortcut cannot drift apart. It
   * lives here because the menu is part of the keymap, not part of the shell.
   * Outside Tauri it is inert.
   */
  useEffect(() => connectMenu(keymap), [keymap]);

  /*
   * Shout about conflicting bindings in development.
   *
   * A conflict never throws — ties go to whichever component mounted last — so
   * without this it shows up as a key that quietly does the wrong thing in one
   * mode. That is precisely how Cmd-1 ended up toggling the first calendar
   * instead of switching to mail.
   *
   * Checked on a short delay after every keypress and mode change, because
   * bindings register on mount and the set that is *live* changes with `when()`.
   */
  useEffect(() => {
    if (!import.meta.env.DEV) return;
    let timer: number | undefined;
    const check = () => {
      window.clearTimeout(timer);
      timer = window.setTimeout(() => {
        for (const c of keymap.conflicts()) {
          console.warn(
            `[keymap] "${c.keys}" is claimed by ${c.bindings.length} live bindings — ` +
              `"${c.bindings[0]?.description ?? "?"}" wins because it registered last. ` +
              `Others: ${c.bindings.slice(1).map((b) => `"${b.description ?? "?"}"`).join(", ")}`,
          );
        }
      }, 250);
    };
    check();
    window.addEventListener("keydown", check, true);
    return () => {
      window.clearTimeout(timer);
      window.removeEventListener("keydown", check, true);
    };
  }, [keymap]);

  return <KeymapContext.Provider value={keymap}>{children}</KeymapContext.Provider>;
}

export function useKeymap(): Keymap {
  const keymap = useContext(KeymapContext);
  if (!keymap) throw new Error("useKeyBindings must be used inside <KeymapProvider>");
  return keymap;
}

/**
 * Register a feature's bindings for as long as the component is mounted.
 *
 * Handlers are read through a ref, so they always see fresh props and state
 * without churning the registry on every render. The registry is only rebuilt
 * when the *shape* of the list changes (keys, priority, input policy).
 */
export function useKeyBindings(bindings: KeyBinding[]): void {
  const keymap = useKeymap();
  const latest = useRef(bindings);
  latest.current = bindings;

  const signature = bindings
    .map(
      (b) =>
        `${b.keys}/${b.priority ?? 0}/${b.allowInInput ? 1 : 0}/${b.group ?? ""}/${b.description ?? ""}/${b.alsoKeys?.join(" ") ?? ""}`,
    )
    .join("|");

  useEffect(() => {
    const unregister = latest.current.map((binding, index) =>
      keymap.register({
        keys: binding.keys,
        description: binding.description,
        group: binding.group,
        alsoKeys: binding.alsoKeys,
        priority: binding.priority,
        allowInInput: binding.allowInInput,
        preventDefault: binding.preventDefault,
        passthrough: binding.passthrough,
        when: () => latest.current[index]?.when?.() ?? true,
        handler: (event) => latest.current[index]?.handler(event),
      }),
    );
    return () => unregister.forEach((fn) => fn());
  }, [keymap, signature]);
}

/** The pending sequence prefix, for the status bar's "g …" affordance. */
export function usePendingSequence(): string | null {
  const keymap = useKeymap();
  const [pending, setPending] = useState<string | null>(null);

  useEffect(() => {
    const tick = () => setPending(keymap.pending());
    window.addEventListener("keydown", tick, false);
    const timer = window.setInterval(tick, 300);
    return () => {
      window.removeEventListener("keydown", tick, false);
      window.clearInterval(timer);
    };
  }, [keymap]);

  return pending;
}
