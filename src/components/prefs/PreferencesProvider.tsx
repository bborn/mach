import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import {
  DEFAULT_PREFERENCES,
  SESSION_WRITE_DEBOUNCE_MS,
  loadPreferences,
  loadSession,
  saveSession,
  writePreference,
  type Preferences,
  type UiSession,
} from "@/lib/prefs";

/**
 * The one copy of the preferences, and the one place they are written from.
 *
 * # Optimistic, always
 *
 * `set` updates the in-memory copy and *then* writes, rather than waiting for
 * the round trip. A settings control that lags a keystroke behind the switch
 * feels broken even when it is working, and the write cannot meaningfully fail:
 * it is one row in a local SQLite file. If it does fail the value stays applied
 * for this session and is simply not there on the next launch, which is a much
 * better outcome than a control that snaps back under the pointer.
 *
 * # Why this sits outside `MachProvider`
 *
 * Because `useMach` reads it. The theme lives here now — `ui.theme` mirrors this
 * rather than owning it — so the provider that answers "what theme" has to be
 * mounted above the one that asks. Nothing here depends on mail or on the
 * keymap, so being outermost costs nothing.
 *
 * # The two side effects
 *
 * Density is applied here, as a `data-density` attribute on the root element,
 * because it is a pure token swap and CSS is where the tokens live (see
 * `globals.css`). Everything else is applied at its point of use, so that a
 * preference and the behaviour it names cannot drift apart across a file
 * boundary — see the table in `lib/prefs.ts`.
 */

interface PreferencesValue {
  prefs: Preferences;
  /** Write one preference. Applies immediately; persists in the background. */
  set: <K extends keyof Preferences>(key: K, value: Preferences[K]) => void;
  /**
   * Whether the stored values have arrived yet.
   *
   * The dialog uses it to avoid rendering a form full of defaults for the one
   * frame before the real answers land — which would otherwise read as "my
   * settings were lost". The session restorer uses it for a load-bearing
   * reason: it must not write "the app is in the mail mode with a 520px
   * divider" over the stored session before the stored session has arrived.
   */
  loaded: boolean;
  /**
   * Where the window was last time, restored. A field is absent when nothing
   * usable was stored for it — see {@link parseSession}.
   */
  session: Partial<UiSession>;
  /**
   * Remember part of where the window is now.
   *
   * Merges rather than replaces, so two unrelated components (the shell and the
   * calendar sidebar) can each remember their own thing without knowing about
   * the other's. Debounced: see {@link SESSION_WRITE_DEBOUNCE_MS}.
   */
  remember: (patch: Partial<UiSession>) => void;
}

const PreferencesContext = createContext<PreferencesValue | null>(null);

/**
 * Set a preference from outside React — what the ⌘K resolver dispatches.
 *
 * The palette's resolvers are plain functions with no hooks and no context, for
 * the same reason the composer and the plugins panel are reached by event: they
 * run in a module that must not import half the component tree. `detail` is a
 * partial `Preferences`, and every key in it is written.
 */
export const PREFERENCE_SET_EVENT = "mach:preference-set";

export function setPreferenceFromAnywhere(patch: Partial<Preferences>): void {
  window.dispatchEvent(new CustomEvent(PREFERENCE_SET_EVENT, { detail: patch }));
}

export function PreferencesProvider({ children }: { children: ReactNode }) {
  const [prefs, setPrefs] = useState<Preferences>(DEFAULT_PREFERENCES);
  const [session, setSession] = useState<Partial<UiSession>>({});
  const [loaded, setLoaded] = useState(false);

  useEffect(() => {
    let live = true;
    void Promise.all([loadPreferences(), loadSession()]).then(([stored, where]) => {
      if (!live) return;
      setPrefs(stored);
      setSession(where);
      // Last, and after both: `loaded` is the flag the restorer waits on before
      // it starts writing, so flipping it early would let a default overwrite
      // the thing it was about to restore.
      setLoaded(true);
    });
    return () => {
      live = false;
    };
  }, []);

  /*
   * The session write, debounced.
   *
   * The pending value lives in a ref rather than in state because the dominant
   * writer is a pointer drag: putting each intermediate width through React
   * would re-render the shell a few hundred times for one gesture, which is the
   * cost `MailMode`'s resizer already goes to some trouble to avoid.
   *
   * `session` state is still updated, because the sidebar reads its own field
   * back — but it is updated with the merged object, so a render is one object
   * identity change per change, not per keystroke of the pointer.
   */
  const pending = useRef<Partial<UiSession>>({});
  const timer = useRef<number | null>(null);

  useEffect(
    () => () => {
      // Unmount is the last chance: flush rather than lose the drag that was
      // in flight when the window closed.
      if (timer.current !== null) {
        window.clearTimeout(timer.current);
        void saveSession(pending.current).catch(() => {});
      }
    },
    [],
  );

  const remember = useCallback((patch: Partial<UiSession>) => {
    setSession((previous) => {
      const next = { ...previous, ...patch };
      pending.current = next;
      return next;
    });
    if (timer.current !== null) window.clearTimeout(timer.current);
    timer.current = window.setTimeout(() => {
      timer.current = null;
      void saveSession(pending.current).catch(() => {
        /* where the divider was is not worth a message on screen */
      });
    }, SESSION_WRITE_DEBOUNCE_MS);
  }, []);

  const set = useCallback(<K extends keyof Preferences>(key: K, value: Preferences[K]) => {
    setPrefs((previous) => (previous[key] === value ? previous : { ...previous, [key]: value }));
    void writePreference(key, value).catch(() => {
      /* see the note on optimism above */
    });
  }, []);

  useEffect(() => {
    const apply = (event: Event) => {
      const patch = (event as CustomEvent<Partial<Preferences>>).detail;
      if (!patch) return;
      for (const [key, value] of Object.entries(patch)) {
        set(key as keyof Preferences, value as Preferences[keyof Preferences]);
      }
    };
    window.addEventListener(PREFERENCE_SET_EVENT, apply);
    return () => window.removeEventListener(PREFERENCE_SET_EVENT, apply);
  }, [set]);

  // Density is a token swap and nothing more: the attribute selects a different
  // `--row-height` and type ramp, and every component that already reads those
  // reflows without knowing a preference exists.
  useEffect(() => {
    document.documentElement.dataset.density = prefs.density;
  }, [prefs.density]);

  const value = useMemo<PreferencesValue>(
    () => ({ prefs, set, loaded, session, remember }),
    [prefs, set, loaded, session, remember],
  );

  return <PreferencesContext.Provider value={value}>{children}</PreferencesContext.Provider>;
}

/**
 * The preferences, wherever you are.
 *
 * Falls back to the defaults rather than throwing when there is no provider
 * above — a component tree in a test does not have to mount the whole app to
 * render a thread row, and defaulting is exactly what "nobody has configured
 * anything" means.
 */
export function usePreferences(): Preferences {
  return useContext(PreferencesContext)?.prefs ?? DEFAULT_PREFERENCES;
}

/** The provider's full surface. Only the dialog and the restorer need it. */
export function usePreferencesStore(): PreferencesValue {
  const value = useContext(PreferencesContext);
  if (!value) throw new Error("usePreferencesStore must be used inside <PreferencesProvider>");
  return value;
}

/**
 * The session half, for components that remember one thing about themselves.
 *
 * Defaults to "nothing stored, remembering is a no-op" outside a provider, for
 * the same reason `usePreferences` defaults: a component in a test should not
 * have to mount the app to render.
 */
export function useUiSession(): { session: Partial<UiSession>; remember: PreferencesValue["remember"] } {
  const value = useContext(PreferencesContext);
  return useMemo(
    () => ({ session: value?.session ?? EMPTY_SESSION, remember: value?.remember ?? noop }),
    [value],
  );
}

const EMPTY_SESSION: Partial<UiSession> = {};
function noop() {}
