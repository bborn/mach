import { FolderOpen, Plus, Trash2 } from "lucide-react";
import { useEffect, useId, useRef, useState } from "react";
import { useKeyBindings } from "@/hooks/useKeymap";
import {
  PLACEHOLDERS,
  draftTarget,
  nameFromDir,
  pickDirectory,
  saveTargets,
  targetProblem,
  targetSnapshot,
  type HandoffMode,
  type HandoffTarget,
} from "@/lib/handoff";
import { errorMessage } from "@/lib/ipc";
import { cn } from "@/lib/utils";
import { Overlay } from "@/components/ui/dialog";
import { Button } from "@/components/ui/button";
import { Field, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { Kbd } from "@/components/ui/kbd";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";

const MODES: { value: HandoffMode; label: string }[] = [
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
        <span className="ml-auto flex shrink-0 items-center gap-1">
          <Kbd keys="escape" />
          <span className="text-micro text-faint-foreground">close</span>
        </span>
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
          (target.mode === "terminal"
            ? "Opens a session in your terminal"
            : "Runs and shows what it printed")}
      </p>
    </div>
  );
}
