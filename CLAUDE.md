# Working on Mach

## Interface copy is not prose

Comments in this codebase are deliberately discursive — they explain *why*, at
length, because the reasoning behind a decision is the expensive part to
recover. **Interface copy is the opposite.** Nobody is reading the preferences
window for pleasure; they are looking for the control they came for, and every
sentence between them and it is a cost.

So: labels, not sentences. Help text only where the control is genuinely
ambiguous, and then as a fragment, not an explanation.

| Don't | Do |
|---|---|
| "Compact tightens the thread row and the type scale with it — a few more conversations on screen, at the cost of some air." | *(nothing — "Compact" is self-evident)* |
| "How long ⌘Z stays offered after a command. One keystroke can archive fifty conversations; this is how long you have to notice." | "How long ⌘Z stays available" |
| "A sent message waits this long in the outbox, recallable with ⌘Z, before it leaves." | "Delay before a message leaves" |
| "Only used when there is nothing to infer from — a reply already knows its account, and so does a list filtered to one." | "Used when the account can't be inferred" |

The rule of thumb: if the help text explains the *reasoning*, it belongs in a
code comment. If it names the *effect*, it can stay — briefly.

### Never narrate the software's own behaviour

Status lines like "Saved as you change them", "Syncing your mail…", "All
changes saved" are the software talking about itself. Preferences that save
themselves should simply do so. Reserve messages for things the user must act
on, or would otherwise not know.

## Published prose has its own rule

This is separate from the interface-copy rule above. It covers anything written
for a reader outside the app: `docs/index.html` (the machmail.dev site),
`README.md`, everything under `docs/`, and also commit messages, code comments
and prompts, which is where these habits come from.

State the fact and stop. The facts here are good enough to carry themselves;
none of them needs a frame, a reveal, or a sentence telling the reader what to
conclude.

Never write these:

| Pattern | Example that shipped |
|---|---|
| "X, not Y" — the contrarian reframe | "That is a decision, not a gap." |
| Aphorisms that sound profound | "saying so is the most useful thing on this page" |
| Meta-commentary about the page itself | "the most useful thing on this page" |
| "deliberately", "on purpose", "by design" as a badge | "What it deliberately isn't" |
| Em-dash as a dramatic pause before a reveal | "Opening a thread is a local read — not a request" |
| Escalating triples | "Local first, keyboard first, fast" |
| Sentence fragments for emphasis | "Local first. Always." |
| Telling the reader what to conclude | "and it is not a failure" |

Banned words and phrases, by name:

- **"worth saying plainly"**, "worth noting", "to be clear", "let me be honest".
  Throat-clearing in front of a sentence that should just start.
- **"genuinely"**, as in "genuinely useful", "genuinely good". If the claim needs
  the intensifier, the claim is weak.
- **"load-bearing"** as a compliment about prose or design.
- **"and here's where…"**, "here's the thing", "the interesting part is".
  Announcing that something interesting is coming instead of saying it.

The register to avoid is confident, chummy and self-satisfied. The fix is
plainer, not more formal; do not overcorrect into corporate stiffness.

Before/after, from the rewrite that produced this rule:

> **Before:** It talks to the Google APIs directly and to nothing else — no
> IMAP, no Outlook, no Fastmail. That is a decision, not a gap.
>
> **After:** It talks to the Gmail and Google Calendar APIs. There is no IMAP
> and no support for other mail providers.

## Look at your work before you call it done

A screenshot of the fixture browser is not a screenshot of the app. `bun run
dev` renders in a browser tab: no traffic lights, no overlay title bar, no
native window chrome, and a different scrollbar engine. It is fine for logic and
for most component work, and it is **not** evidence for anything that fills the
window or sits near an edge.

The preferences surface shipped with its title underneath the macOS
close/minimise/zoom buttons. It had been screenshotted — in the browser, where
those buttons do not exist.

So: **if a change touches full-window layout, a window edge, or anything the OS
also draws, verify it in the real app**:

```sh
MACH_QA_INSTANCE=agent scripts/qa up        # Accessory policy: never takes focus
MACH_QA_INSTANCE=agent scripts/qa shoot after
```

The window is `titleBarStyle: "Overlay"` with `hiddenTitle`, so macOS paints the
traffic lights over the top-left of *our* content. Anything filling the window
owes them `pl-[5.5rem]` — `chrome/TitleBar.tsx` is the reference.

And "the tests pass" is not the same claim as "it looks right". Both are
required; neither substitutes for the other.

## Other standing rules

- **Everything is keyboard navigable.** No mouse-only affordances, ever.
- **Keyboard shortcuts match Gmail and Google Calendar** where those have one.
  Do not invent a vocabulary the user has to learn twice.
- **The UI never waits on Google.** Everything renders from local SQLite;
  network is a background loop that writes into it. Optimistic writes with an
  exact inverse, rolled back on failure.
- **Failure must be visible.** A write that Google refuses has to say so. Silent
  failure is the specific thing that has cost this project the most time.
- **Use the primitives in `src/components/ui/`.** No native `<select>`,
  `<input type=checkbox>`, or hand-rolled equivalents.
