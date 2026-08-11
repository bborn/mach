import { FolderOpen, Plus, Trash2 } from "lucide-react";
import { useEffect, useId, useRef, useState } from "react";
import { useKeyBindings } from "@/hooks/useKeymap";
import {
  NO_TERMINALS,
  OTHER_TERMINAL,
  PLACEHOLDERS,
  draftTarget,
  loadTerminals,
  nameFromDir,
  pickDirectory,
  saveTargets,
  targetProblem,
  targetSnapshot,
  terminalFromSelection,
  terminalItems,
  terminalSelection,
  type HandoffMode,
  type HandoffTarget,
  type Terminals,
} from "@/lib/handoff";
import { errorMessage } from "@/lib/ipc";
import { cn } from "@/lib/utils";
import { usePreferencesStore } from "@/components/prefs/PreferencesProvider";
import { Overlay } from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { Field, FieldDescription, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";

const MODES: { value: HandoffMode; label: string }[] = [
  { value: "session", label: "Session" },
  { value: "terminal", label: "Terminal" },
  { value: "inline", label: "Inline" },
];

const MODE_ITEMS = Object.fromEntries(MODES.map((m) => [m.value, m.label]));

/**
 * Where handoffs can go.
 *
 * Four fields per row, a list of rows, and nothing else. It is not in
 * preferences on purpose: `prefs.ts` is a flat bag of values and this is a
 * small table with its own validation, its own key and its own commands. ⌘K →
 * "Handoff targets…" is the only way in, which is the same route the thing it
 * configures is used from.
 *
 * On the very first open there is nothing to edit, so it asks the one question
 * that makes a useful target — which directory — and fills the rest in. Zero
 * configuration ends with a working `claude "{{prompt}}"` in a repo he named
 * rather than with an empty list and a form.
 */
export function HandoffTargetsDialog({ onClose }: { onClose: () => void }) {
  const [rows, setRows] = useState<HandoffTarget[]>(() => {
    const stored = targetSnapshot();
    return stored.length > 0 ? stored.map((t) => ({ ...t })) : [];
  });
  const [failure, setFailure] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const seeded = useRef(false);

  // The first-run question. Asked once, and only when there is nothing at all.
  useEffect(() => {
    if (seeded.current || rows.length > 0) return;
    seeded.current = true;
    void pickDirectory()
      .then((dir) => {
        if (dir) setRows([draftTarget(dir)]);
      })
      .catch((error: unknown) => setFailure(errorMessage(error)));
    // Runs once per mount; `rows` changing must not re-ask.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // A failure describes the list as it was when it failed. Touching the list
  // makes it stale, and a red line about a save he has already moved past is
  // just noise on the next attempt.
  useEffect(() => setFailure(null), [rows]);

  const problems = rows.map(targetProblem);
  const canSave = problems.every((p) => p === null) && !saving;

  const save = async () => {
    if (!canSave) return;
    setSaving(true);
    setFailure(null);
    try {
      await saveTargets(rows);
      onClose();
    } catch (error) {
      setFailure(`Nothing was saved. ${errorMessage(error)}`);
    } finally {
      setSaving(false);
    }
  };

  useKeyBindings([
    {
      keys: "escape",
      group: "Global",
      description: "Close handoff targets",
      allowInInput: true,
      priority: 310,
      handler: () => onClose(),
    },
    {
      keys: "mod+enter",
      group: "Global",
      description: "Save handoff targets",
      allowInInput: true,
      priority: 310,
      when: () => canSave,
      handler: () => void save(),
    },
  ]);

  const update = (index: number, patch: Partial<HandoffTarget>) =>
    setRows((current) => current.map((row, i) => (i === index ? { ...row, ...patch } : row)));

  return (
    <Overlay open onClose={onClose} labelledBy="handoff-targets-title" className="max-w-[46rem]">
      <header className="flex h-10 shrink-0 items-center gap-2 border-b border-border px-3">
        <span id="handoff-targets-title" className="text-body text-foreground">
          Handoff targets
        </span>
        {/* No `Esc close`: the footer of this dialog has Cancel and Done. */}
      </header>

      <div className="flex min-h-0 flex-col gap-2 overflow-y-auto p-3">
        {rows.length === 0 && (
          <p className="text-list text-muted-foreground">
            No targets yet. Add one, or press ⌘K again later.
          </p>
        )}

        {rows.map((row, index) => (
          <TargetRow
            key={row.id || `draft-${index}`}
            target={row}
            problem={problems[index] ?? null}
            onChange={(patch) => update(index, patch)}
            onRemove={() => setRows((current) => current.filter((_, i) => i !== index))}
          />
        ))}

        <Button
          size="sm"
          className="self-start"
          onClick={() => setRows((current) => [...current, draftTarget()])}
        >
          <Plus size={12} strokeWidth={1.75} />
          Add target
        </Button>

        <div className="mt-1 border-t border-border pt-2.5">
          <TerminalChoice />
        </div>

        {failure && <p className="text-list text-danger">{failure}</p>}
      </div>

      <footer className="flex h-9 shrink-0 items-center gap-2 border-t border-border px-3">
        <span className="truncate font-mono text-micro text-faint-foreground">
          {PLACEHOLDERS.map((name) => `{{${name}}}`).join("  ")}
        </span>
        <span className="ml-auto flex shrink-0 items-center gap-1.5">
          <Button size="sm" onClick={onClose}>
            Cancel
          </Button>
          <Button size="sm" variant="default" disabled={!canSave} onClick={() => void save()}>
            Done
          </Button>
        </span>
      </footer>
    </Overlay>
  );
}

/**
 * Which terminal `Terminal` mode means.
 *
 * One control for the whole app rather than one per row: a person has one
 * terminal, and a column repeating the same word in every target would be a
 * column that can only ever disagree with itself. It lives here rather than in
 * ⌘, because this is the surface where `Terminal` is chosen, and splitting one
 * feature's configuration across two dialogs is how a setting becomes
 * unfindable. The value itself is an ordinary preference — see
 * `handoffTerminalApp` in `lib/prefs.ts`.
 *
 * The menu offers what is installed, found by looking for the bundles rather
 * than by listing names. Eight terminals in a menu on a Mac that has two is a
 * menu you have to read; `Other…` is underneath for a build kept somewhere
 * macOS does not look, and takes a name or a path. A name that resolves to
 * nothing fails at launch with a sentence naming it — `open` opens nothing and
 * says so, and `handoff::plan` passes that through.
 */
function TerminalChoice() {
  const { prefs, set, loaded } = usePreferencesStore();
  const [terminals, setTerminals] = useState<Terminals>(NO_TERMINALS);
  const [detected, setDetected] = useState(false);
  // Whether the text field is open. Not derivable from the value: `iTerm` is a
  // detected name whether he picked it from the menu or typed it.
  const [custom, setCustom] = useState(false);
  const settled = useRef(false);
  const id = useId();

  useEffect(() => {
    let live = true;
    void loadTerminals().then((next) => {
      if (!live) return;
      setTerminals(next);
      setDetected(true);
    });
    return () => {
      live = false;
    };
  }, []);

  // Which row is selected on arrival, decided once both halves have landed —
  // asking before the terminals are known would read a configured `iTerm` as
  // "something I have never heard of" and open the text field over it.
  useEffect(() => {
    if (settled.current || !loaded || !detected) return;
    settled.current = true;
    setCustom(terminalSelection(prefs.handoffTerminalApp, terminals.installed) === OTHER_TERMINAL);
  }, [loaded, detected, prefs.handoffTerminalApp, terminals.installed]);

  const stored = prefs.handoffTerminalApp;
  const items = terminalItems(terminals.installed);
  const selection = custom ? OTHER_TERMINAL : terminalSelection(stored, terminals.installed);

  // The environment variable predates this control and still wins, so the
  // control says so rather than offering a choice that would not be applied.
  if (terminals.forced) {
    return (
      <Field orientation="row">
        <FieldLabel htmlFor={id}>Terminal</FieldLabel>
        <Input id={id} value={terminals.forced} readOnly className="w-[13rem] font-mono" />
        <FieldDescription>Set by MACH_HANDOFF_TERMINAL_APP</FieldDescription>
      </Field>
    );
  }

  return (
    <Field orientation="row">
      <FieldLabel htmlFor={id}>Terminal</FieldLabel>
      <span className="flex min-w-0 items-center gap-1.5">
        <span className={cn("shrink-0", custom ? "w-[9rem]" : "w-[13rem]")}>
          <Select
            items={items}
            value={selection}
            onValueChange={(next) => {
              if (next === null) return;
              setCustom(next === OTHER_TERMINAL);
              set("handoffTerminalApp", terminalFromSelection(next, stored));
            }}
          >
            <SelectTrigger id={id} aria-label="Terminal">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {items.map((item) => (
                <SelectItem key={item.value} value={item.value}>
                  {item.label}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </span>
        {custom && (
          <Input
            aria-label="Application name or path"
            spellCheck={false}
            placeholder="/Applications/iTerm.app"
            className="font-mono"
            value={stored}
            onChange={(event) => set("handoffTerminalApp", event.target.value)}
          />
        )}
      </span>
    </Field>
  );
}

function TargetRow({
  target,
  problem,
  onChange,
  onRemove,
}: {
  target: HandoffTarget;
  problem: string | null;
  onChange: (patch: Partial<HandoffTarget>) => void;
  onRemove: () => void;
}) {
  const ids = {
    name: useId(),
    dir: useId(),
    run: useId(),
    mode: useId(),
  };

  const choose = () => {
    void pickDirectory()
      .then((dir) => {
        if (!dir) return;
        // A row that has not been named yet takes the directory's name; one he
        // has already named keeps it.
        onChange(target.name.trim() ? { dir } : { dir, name: nameFromDir(dir) });
      })
      .catch(() => undefined);
  };

  return (
    <div className="flex flex-col gap-1.5 rounded-[var(--radius)] border border-border p-2">
      <div className="grid grid-cols-[minmax(0,1fr)_minmax(0,2fr)_auto] items-start gap-2">
        <Field orientation="vertical">
          <FieldLabel htmlFor={ids.name}>Name</FieldLabel>
          <Input
            id={ids.name}
            value={target.name}
            placeholder="OfferLab"
            onChange={(event) => onChange({ name: event.target.value })}
          />
        </Field>

        <Field orientation="vertical">
          <FieldLabel htmlFor={ids.dir}>Directory</FieldLabel>
          <span className="flex items-center gap-1.5">
            <Input
              id={ids.dir}
              value={target.dir}
              placeholder="~/Projects/offerlab"
              className="font-mono"
              onChange={(event) => onChange({ dir: event.target.value })}
            />
            <Button size="sm" onClick={choose} aria-label="Choose a directory">
              <FolderOpen size={12} strokeWidth={1.75} />
            </Button>
          </span>
        </Field>

        <Button
          size="sm"
          className="mt-[1.35rem]"
          onClick={onRemove}
          aria-label={`Remove ${target.name || "target"}`}
        >
          <Trash2 size={12} strokeWidth={1.75} />
        </Button>
      </div>

      <div className="grid grid-cols-[minmax(0,1fr)_9rem] items-start gap-2">
        <Field orientation="vertical">
          <FieldLabel htmlFor={ids.run}>Command</FieldLabel>
          <Input
            id={ids.run}
            value={target.run}
            placeholder={'claude "{{prompt}}"'}
            className="font-mono"
            onChange={(event) => onChange({ run: event.target.value })}
          />
        </Field>

        <Field orientation="vertical">
          <FieldLabel htmlFor={ids.mode}>Mode</FieldLabel>
          <Select
            items={MODE_ITEMS}
            value={target.mode}
            onValueChange={(value) => onChange({ mode: value as HandoffMode })}
          >
            <SelectTrigger id={ids.mode} aria-label="Mode">
              <SelectValue />
            </SelectTrigger>
            <SelectContent>
              {MODES.map((mode) => (
                <SelectItem key={mode.value} value={mode.value}>
                  {mode.label}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </Field>
      </div>

      <p className={cn("text-micro", problem ? "text-danger" : "text-faint-foreground")}>
        {problem ??
          (target.mode === "session"
            ? "Opens a session in a pane here"
            : target.mode === "terminal"
              ? "Opens a session in your terminal"
              : "Runs and shows what it printed")}
      </p>
    </div>
  );
}
