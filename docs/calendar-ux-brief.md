# Calendar UX brief

Implementation spec for Mach's calendar. Written to the instruction "don't invent your
own calendaring UX — find the best thing out there and copy it."

Where a number is marked **[measured]** it was read off a live app's DOM, not inferred.
Google Calendar measurements were taken from the running web app in Chrome at 100% zoom
(window 1508 CSS px wide), August 2026. Everything else is cited to its source.

Open questions for the author are collected in the last section. Everything else is decided.

---

## 0. Headline decisions

1. **Google Calendar is the geometry reference.** Not because it's the best app, but
   because it is the one most people's muscle memory is already calibrated to, and its numbers
   are demonstrably tuned. Copy its grid metrics exactly.
2. **Notion Calendar is the keyboard reference.** Single-letter, no-modifier bindings,
   near-identical to Google's own map. Copy it; it is a superset of what Google already trained
   everyone on.
3. **Fantastical is the event-entry reference.** Its natural-language grammar is the
   most mature in the category and is publicly documented with worked examples.
4. **The overlap algorithm already in `src/lib/event-layout.ts` is correct — keep it.**
   It is, line for line, the canonical accepted answer to this problem. Details in §2.
   This is the one place where "designed from first principles" happened to land on
   exactly the industry-standard answer.
5. **Amie is no longer a valid reference.** See §9.

---

## 1. The week grid

### Hour row height

**48px per hour.** [measured] Google Calendar's hour labels sit at exactly 48px pitch
(10 AM at y=304, 11 AM at y=352).

`TimeGrid.tsx` already uses `HOUR_HEIGHT = 48`. **No change.** Do not make this
configurable in v1 — 48 is the number that makes a 30-minute meeting exactly 24px, which
is the minimum height that fits a 15px text line plus 4px of padding top and bottom.

Derived constants:

| Duration | Height | What fits |
|---|---|---|
| 15 min | 12px | one line, overflowing the block (see below) |
| 30 min | 24px | one line: title + time, comma-separated |
| 45 min | 36px | two lines: title, then time |
| 60 min | 48px | two lines + breathing room |

### The time gutter

**56px wide**, labels right-aligned with an 8px right inset. [measured] Google's hour
label is 43px wide sitting in a 56px gutter, grid content starts at x=324 with labels
at x=264.

Label type: **11px / 16px line-height, weight 500**, colour `#444746` in light mode.
[measured] Note it is *not* the same size as event text — the axis is deliberately one
step smaller and one step greyer than the content.

Labels mark the **top** of their hour and are vertically centred on the gridline, not
sitting below it. Do not render a label for hour 0 (it collides with the all-day row).

### Gridlines

One 1px line per hour at `--border`. **Do not draw 30-minute lines.** Google doesn't,
and the current implementation's
`repeating-linear-gradient(to bottom, var(--border) 0 1px, transparent 1px 48px)`
is already right. Half-hour lines double the visual noise and buy nothing, because
event blocks already communicate their own boundaries.

### Current-time indicator

Copy Google exactly. [measured]

- **Line:** 2px solid, `#DB372D`, `z-index` above events.
- **Dot:** 12px circle, same colour, `border-radius: 9999px`, vertically centred on the
  line, horizontally offset **−6px** so it straddles the left edge of the day column
  (it hangs into the gutter).
- **Scope: today's column only.** Google does *not* run the line across the whole week.
  This is correct and worth being deliberate about — a full-width rule reads as a
  divider, a column-width rule reads as "you are here". The current implementation
  already scopes it per-column.
- **Update cadence:** once a minute is enough (current code uses `60_000` — keep it).
  Per-second updates burn wakeups for sub-pixel movement (1px ≈ 75 seconds at 48px/hr).

`z-index`: Google uses 507 for the indicator vs 5 for event blocks. [measured] Use two
named layers rather than magic numbers: events at 5, now-line at 20.

### The "now" scroll position

Google's week view opens with roughly **07:00 at the top of the viewport** — it does
*not* centre on the current time. [measured/observed]

The current implementation does `scrollTop = 7.5 * HOUR_HEIGHT`, i.e. 07:30 at top.
**Change this to a rule, not a constant:**

```
scrollTop = clamp(
  nowOffsetPx - 0.25 * viewportHeight,   // now sits ¼ down the viewport
  6.5 * 48,                              // never start earlier than 06:30
  24 * 48 - viewportHeight               // never overscroll the bottom
)
```

Reasoning: a fixed 07:00 is right at 09:00 and useless at 16:00 — you would have to
scroll every single time you open the app after lunch. Anchoring "now" a quarter of the
way down keeps the next few hours (the part you actually act on) in the largest part of
the viewport, while still showing what just happened. The lower clamp stops an early
morning from opening on a wall of empty night hours.

Scroll **without animation** on mount, and with `behavior: "smooth"` when triggered by
the `T` (today) key. Instant on mount avoids a visible lurch; animated on keypress
confirms the key registered.

### All-day row

Copy Google. [measured]

- Chip height **22px**, row pitch **24px** (22 + 2px gap).
- `border-radius: 6px`, padding `0 8px`.
- Type: **12px / 20px, weight 500**, white on the calendar's fill colour.
- The row sits between the day header and the scrollable grid, and **does not scroll
  with the grid** — it is pinned. This matters: an all-day event that scrolls away is an
  all-day event you forget about.
- **Collapse rule:** show at most **3** rows. Beyond that, render a `+N more` affordance
  on the last row which expands the section in place (pushing the grid down), not a
  popover. Google uses a popover here and it is one of the more annoying things about
  its week view — the popover covers the grid you were trying to compare against.

### Short-event legibility — the important trick

This is the single most valuable thing to copy, and it is not obvious.

A 15-minute event in Google is an **11px-tall block containing a 15px-tall text line
that deliberately overflows the block's bounds.** [measured] `overflow: visible`,
`white-space: nowrap`. The coloured rectangle is 11px; the text spills 2px above and
below it and stays fully legible.

Most implementations clip the text to the block and end up with a sliver of a letterform.
Google's approach means a 15-minute event is *always* readable regardless of how thin
the block is.

Implement as: block height = `max(durationPx - 1, 11)`, text line rendered at 15px
line-height, absolutely positioned, vertically centred on the block, `overflow: visible`,
`white-space: nowrap`, `text-overflow: clip`.

The current `MIN_BLOCK_HEIGHT = 17` is a workaround for not doing this. **Lower it to
11 and let the text overflow instead.** 17px forces a 15-minute event to render 40%
taller than its true duration, which is a lie about the geometry — at 17px a 15-minute
event looks the same size as a 21-minute one.

### Short-event pointer targets

Added after the report "the half hour event blocks are too tiny — they need a min height
that's reasonable to click on."

The height stays. The *target* does not have to match it: a block shorter than
`MIN_HIT_HEIGHT` (32px) carries a transparent hit area above and below its painted body,
centred on it, reaching at most `MAX_HIT_OVERHANG` (8px) either way. A 30-minute block
goes from 23px to 33px, a 15-minute one from 11px to 27px. Nothing is drawn and nothing
moves.

The ceiling is there because the strip of grid just under a short meeting is where you
press to drag-create the thing after it. Nothing answers the pointer more than 8px from
where it is drawn.

Two rules keep it from taking clicks that belong elsewhere:

- **Every hit area sits below every painted block** (`Z_EVENT_HIT` under `Z_EVENT`), so a
  hit area reaching into the next block passes underneath it. A 09:00 block cannot take a
  press on the 09:30 one.
- **It grows vertically only.** Left and width come from the block, so side-by-side
  columns keep their boundary.

What it costs: the few pixels of grid above and below a short block no longer start a
drag-to-create. The 13px right-hand gutter below is still clear on every column.

The same 32px is why the resize handles' floor is `24 - BLOCK_GAP` and not 24. A
30-minute block renders at 23px, so a raw 24 excluded the most common meeting length
there is from mouse resize entirely.

---

## 2. Overlapping events

### What the best apps actually do

The canonical algorithm — the accepted, top-voted answer to Stack Overflow's
"Visualization of calendar events. Algorithm to layout events with maximum width"
(question 11311410, accepted answer, 73 votes) — is a three-pass method:

1. Think of an unlimited grid with just a left edge. Each event is one cell wide;
   height and vertical position are fixed by start/end times.
2. Place each event in a column as far left as possible, without intersecting any
   earlier event in that column.
3. When each connected group of events is placed, actual widths are `1/n` of the maximum
   number of columns used by the group.
4. Optionally expand events at the far left and right to use up any remaining space.

### Mach already implements this

`src/lib/event-layout.ts` does exactly this: sort by start ascending then end descending,
cut into clusters at gaps, greedy leftmost-column assignment, then widen rightwards into
free columns. That is the standard algorithm with the expansion pass included.

**Recommendation: keep it. Do not rewrite.** This is the rare case where building from
first principles converged on the same answer as the field. `react-big-calendar` (MIT)
ships the same thing as its `no-overlap` day-layout algorithm.

### Side-by-side columns vs Google's offset-and-stack

The two schools:

- **Equal columns** (Fantastical, Outlook, Apple Calendar, `react-big-calendar`'s
  `no-overlap`, and Mach today): overlapping events split the width evenly. Every event
  is fully visible. Nothing is ever hidden.
- **Offset with z-order** (Google Calendar, `react-big-calendar`'s `overlap`): a shorter
  event nested inside a longer one is drawn *on top of* it, indented from the left, with
  the longer event still visible behind. Reads more compactly for the common
  "a 30-minute call inside a 2-hour focus block" case.

**Pick equal columns. Keep what you have.** Reasoning:

- Google's offset mode has a well-known failure: an event fully covered by a later,
  larger one becomes unclickable and sometimes invisible. This is a real, long-running
  complaint — there is a popular Chrome extension ("GCalPlus") whose entire purpose is
  hovering to bring buried Google Calendar events to the front, and Stack Overflow /
  Google support threads asking how to control event layering. Copying a behaviour that
  spawned a fix-it extension is copying the bug.
- Equal columns degrade predictably. Offset degrades unpredictably — it depends on
  arrival order and containment relationships.
- The stated problem with Spark is that its calendar is unusable. The failure mode
  to avoid at all costs is "the meeting was there and I couldn't see it."

### The escape hatch for dense days — copy Fantastical

Equal columns get thin fast: five overlapping events in a 156px column is 31px each.
Fantastical solves this with a documented interaction:

> "Have a busy day with so many events that they're overlapping each other and you can't
> read the titles? Hold the shift and control keys while moving your mouse across events
> and tasks in the Day and Week view and the items will expand to make the titles
> readable." — Fantastical for Mac, Tips and Tricks (flexibits.com/fantastical/tips)

**Implement a hover-to-expand.** On hover over an event in a cluster of ≥3, that event
animates to full column width at `z-index` above its neighbours, over ~120ms. No modifier
key required — Fantastical's Shift+Control requirement is a wart born of not wanting to
disturb drag behaviour, and Mach can just use plain hover with a short delay (~150ms) to
avoid flicker while the mouse crosses the column.

This gives the compactness benefit of Google's offset mode without its
permanently-buried-event failure.

### Minimum column width

Below **~40px** per column, text is hopeless no matter what. When a cluster would produce
columns narrower than 40px, cap the visible columns and render the overflow as a
`+N` chip in the last column, which opens a day-detail popover listing all of them.
Never render a 12px-wide sliver.

---

## 3. Creating an event

### Both paths, and they converge

Every good client offers click-drag *and* natural language, because they serve different
intents: drag is "this time, duration unknown title", typing is "this thing, time
implied by the sentence".

### Path A — drag on the grid

1. Mouse down on empty grid → immediately render a provisional block at the nearest
   **15-minute** snap.
2. Drag → block grows, snapping to 15 minutes. Show a live "09:15 – 10:00" label inside
   the block.
3. Mouse up → block stays, and an inline title field focuses **in place, inside the
   block** — not in a modal. Typing goes straight into the title.
4. `Enter` saves. `Esc` discards. `Tab` opens the full editor.

A plain **click** (no drag) on empty grid creates a default **30-minute** event at that
snap point and does the same inline-title thing. Do not require a drag for the most
common case.

The inline-in-the-block editor is Notion Calendar's signature move and the reason people
describe it as fast. A modal for a 4-word title is the thing that makes Google Calendar
feel heavy.

### Path B — natural language

Bind to **`C`** (see §4). Opens a single text field, centred, with a live interpretation
preview beneath it showing the parsed date, time, duration and target calendar.

**Copy Fantastical's grammar.** Its documented examples (flexibits.com/fantastical/tips):

```
Grocery shopping at Wegmans Thursday at 5pm
Lunch with Matthew at 123 Main St at 1:30 Monday
Family vacation from August 9-18
Staff meeting Tuesday 2pm alert 20 min
Soccer practice every Tuesday at 6
Sam's birthday every year on 5/16
Pizza party on the 2nd Friday of every month at 1pm
Flight 593 on Monday 3pm EST to 6pm PST
```

Token rules to implement, all from Fantastical's documented behaviour:

| Token | Meaning |
|---|---|
| `at <place>` | location (when not parseable as a time) |
| `with <name>` | invitee — match against contacts as you type |
| `alert <n> min` | reminder offset |
| `every <interval>` | recurrence |
| `from <a> to <b>` / `<a>-<b>` | explicit range |
| `/<calname>` | target calendar, e.g. `/w` → Work |

The `/calendar` prefix matters a lot here — with five accounts, "which
calendar does this land on" is a decision you make on *every* event. A one-keystroke
`/w` is dramatically better than a dropdown. Fantastical also accepts four spaces as an
alternative trigger; skip that, it's undiscoverable.

**Parser: use `chrono-node` (MIT, v2.10.1 — verified on npm).** It handles the date/time
half of the grammar well. Layer the `at`/`with`/`alert`/`/cal` token extraction on top of
it yourself — strip recognised tokens from the string, hand the remainder to chrono, and
whatever chrono doesn't consume becomes the title. Do not write a date parser.

### Fastest path, keystroke by keystroke

Target: **new 30-minute meeting tomorrow at 2pm on the work calendar, in under 25
keystrokes and zero mouse.**

```
C                                    → NL field opens
Standup tomorrow 2pm /w              → 23 chars, live preview confirms
Enter                                → saved, field closes, grid scrolls to it
```

The preview line under the field must resolve *as you type*, so the `Enter` is a
confirmation of something already visible, not a leap of faith. This is what separates
"natural language input" that people trust from the kind they abandon after it silently
schedules something in 2027.

---

## 4. Keyboard model

**Copy Notion Calendar's map**, which is itself a near-superset of Google Calendar's.
Anyone coming from Google Calendar already knows half of it. Single letters, no modifiers, no chords.

### Verified: Google Calendar (official — support.google.com/calendar/answer/37034)

| Key | Action |
|---|---|
| `j` or `n` | next date range |
| `k` or `p` | previous date range |
| `t` | today |
| `r` | refresh |
| `/` | focus search |
| `g` | go to specific date |
| `s` | settings |
| `1` or `d` | day view |
| `2` or `w` | week view |
| `3` or `m` | month view |
| `4` or `x` | custom view |
| `5` or `a` | agenda view |
| `c` | create event |
| `e` | event details |
| `Backspace` / `Delete` | delete event |
| `z` | undo |
| `⌘S` | save (in details) |
| `Esc` | back to grid |

*(The official help table lists only "j or n" for next; `k`/`p` for previous is the
long-standing counterpart and is what the in-app `?` overlay shows.)*

### Verified: Notion Calendar (20 shortcuts)

Notion does not publish the list on the web — it lives behind `?` in-app. The map below
is from ShortcutPosters' transcription (updated Jan 2026, last verified Jul 2026), which
credits notion.com and matthiasfrank.de as sources. **Treat as high-confidence but
unofficial**; verify against the in-app `?` overlay before shipping.

| Key | Action |
|---|---|
| `T` | today |
| `J` | next week / month |
| `K` | previous week / month |
| `←` | navigate backward |
| `→` | navigate forward |
| `N` | next event |
| `B` | previous event |
| `D` | day view |
| `W` | week view |
| `M` | month view |
| `1`…`7` | show N days at a time |
| `⌘K` | show / hide weekends |
| `C` | create new event |
| `S` | share availability |
| `F` | instant call |
| `O` | search / import Notion databases |
| `P` | overlay a teammate's calendar |
| `⇧/` | show all shortcuts |
| `Esc` | dismiss time-travel overlay |

### Verified: Fantastical for Mac (official — flexibits.com/fantastical/tips)

| Key | Action |
|---|---|
| `⌘←` `⌘→` | change dates |
| `⌘F` | search |
| `⌘K` | toggle event ⇄ reminder while creating |
| `⌘N` | new item |
| `⌘E` | show details |
| `⌘T` | today |
| `⇧⌘T` | go to date |
| `⌥⌘S` | toggle sidebar |
| `⌘⌫` | delete selected |

Fantastical is fully modifier-based. **Reject this model.** Modifier chords are the right
call for a Mac app that has to coexist with system text-editing shortcuts everywhere;
they are the wrong call for a grid-focused view where the hands should never leave home
row. Notion Calendar and Google both went bare-letter and both are faster.

### Vimcal — partial

Vimcal markets on speed and keyboard control but does **not** publish a complete shortcut
list; `docs.vimcal.com` has no shortcuts page and the docs site's own navigation contains
no keyboard reference. Confirmed bindings gathered from Vimcal's own docs and newsletter:

| Key | Action |
|---|---|
| `W` | week view |
| `M` | month view |
| `⌘K` | command centre |
| `Z` | time travel (timezone overlay) |

This is consistent with the Google/Notion letter convention. **Do not block on getting
the rest** — the Notion map above is a complete, coherent design and Vimcal appears to be
a variant of the same convention rather than a different idea. Flagged in §10 if it is
worth chasing.

### Mach's map — the decision

Adopt Notion's, with Google's aliases kept as synonyms so existing muscle memory works:

```
Navigation      t / T       today
                j, n        next range
                k, p        previous range
                ← →         previous / next range (arrow synonyms)
                g           go to date (opens date input)
                n / b       next / previous event   ← CONFLICT, see below
Views           d, 1        day
                w, 2        week
                m, 3        month
                1…7         show N days
Events          c           create (natural language field)
                e / Enter   open details for selected
                ⌫ / Del     delete selected
                z           undo
                Esc         deselect / close
Global          /           search
                ⌘K          command palette
                ?           shortcut overlay
```

**One real conflict to resolve:** Google uses `n` for *next date range*; Notion uses `N`
for *next event*. You cannot have both on the bare key.

**Decision: `n`/`p` are date-range navigation (Google's meaning), and event-to-event
navigation moves to `Tab` / `⇧Tab`.** Rationale: range navigation is used an order of
magnitude more often, the existing reflex is Google's, and `Tab` for "next focusable
thing" is what the platform already means. `j`/`k` remain as the vim-flavoured synonym
for range navigation.

**`⌘K` is the command palette, not "toggle weekends".** Notion's choice here is an outlier
and almost certainly a mis-transcription in the poster source; `⌘K` means "command
palette" in every other app on the platform. Weekends toggle goes on the palette.

---

## 5. Density and typography

### The size ladder

Measured off Google Calendar's event blocks: [measured]

| Element | Size / line-height | Weight | Colour |
|---|---|---|---|
| Hour axis label | 11 / 16 | 500 | `#444746` (muted) |
| Event title (normal block) | 12 / 15 | 500 | white on fill |
| Event time (normal block) | 12 / 15 | 400 | white on fill, 85% opacity |
| Event title (compressed, <24px block) | 11 / 15 | 500 | white on fill |
| All-day chip | 12 / 20 | 500 | white on fill |

Two type sizes total in the grid: **12px normal, 11px compressed.** That is the whole
system. Resist adding a third.

### Progressive degradation as a block gets shorter

This is the rule, in order. Each threshold drops exactly one thing:

| Block height | Duration @48px/hr | Render |
|---|---|---|
| ≥ 48px | ≥ 60 min | Title (12px, may wrap to 2 lines) · time on its own line · location if present |
| 34–47px | 45 min | Title (12px, 1 line, ellipsised) · time on its own line |
| 24–33px | 30 min | Title + time on **one** line, comma-separated: `Standup, 11am` |
| 11–23px | ≤ 15 min | Same one line, at **11px**, overflowing the block bounds |

The comma-joined single line is Google's actual behaviour and it is better than the
obvious alternatives. [measured] Dropping the time entirely loses information you need
when blocks are stacked; putting time first buries the title; two clipped half-lines are
illegible. `Title, 11am` reads as natural language and truncates gracefully — the title
ellipsises and the time survives, because the time is what disambiguates two similar
blocks.

**Truncation:** single line, `overflow: hidden`, `text-overflow: ellipsis` on the title
span only. The time span is `flex-shrink: 0` so it never truncates. Never wrap in a block
under 48px.

### Block chrome

- `border-radius: **6px**` [measured] — not 4 (too sharp against 48px rows), not 8
  (eats the corners of a 12px block).
- **Solid fill, no border, no gradient, no shadow** for normal events.
- Right gutter: event width = column width − **13px**. [measured] Google leaves this
  strip deliberately so there is always somewhere to click-drag to create a new event in
  a column that's already full. Copy it — it's the difference between "I can always
  create an event here" and "I have to find a gap".
- 1px vertical gap between stacked blocks (`height - 1`), which the current code
  already does.

---

## 6. Colour

### The core problem

Five accounts × several calendars each = potentially 15+ colours on screen. The failure
mode is a bag of sweets. The fix is not "pick nicer colours", it is **to reduce how many
things colour has to say at once.**

### Rule: colour encodes calendar identity, and nothing else

Do not overload hue with status. Status (accepted / tentative / declined / past) is
encoded by **fill treatment**, not by hue. This is the single most important colour
decision and it's what Google gets right.

### Fill treatments — copy Google exactly

Measured from a live Google Calendar: [measured]

| State | Treatment |
|---|---|
| **Accepted / you own it** | Solid fill in the calendar colour, white text |
| **Invited, not yet responded** | **White fill**, title in `#1F1F1F`, time and 1px border in the calendar colour |
| **Past** | Same fill, reduced to ~60% opacity |
| **Declined** | Hidden by default; when shown, outlined with strikethrough title |

The white-fill-for-unanswered treatment is excellent and under-copied. It makes "things I
haven't dealt with" pop out of a dense week without introducing a new hue, and it
naturally reads as less committed than a solid block. [measured — observed directly on a
pending invitation in the live app]

**Past events at 60% opacity, not greyscale.** Desaturating to grey destroys the calendar
identity you spent the hue budget on. Opacity keeps the hue readable while pushing it
back. Apply to the whole block including text.

### Palette

Google's event palette is 11 named colours. The one measured in the wild:
`#039BE5` (Peacock) for a timed event, `#0B8043` (Basil) for an all-day chip. [measured]

**Recommendation:** do not use Google's palette values directly. They are tuned for white
text on solid fill at Google's exact type weights and several of them (Banana, Flamingo)
have poor contrast at 11px. Instead:

- Define **8 hues**, evenly spaced in hue, at a fixed lightness/chroma in OKLCH so they
  are perceptually equal in weight. Something like `oklch(0.62 0.15 <h>)` for
  `h ∈ {25, 70, 115, 160, 205, 250, 295, 340}`.
- Fixed lightness is the whole trick. A palette looks like confetti when the colours vary
  in *lightness*, not when they vary in hue — uneven lightness makes some events shout.
- Verify each against white text at 11px/500 for ≥4.5:1 contrast; nudge lightness down
  globally (not per-hue) until all pass.
- Dark mode: same hues, `oklch(0.55 0.13 <h>)` fills with `oklch(0.92 ...)` text, or
  invert to tinted-dark fills with light text. Keep lightness uniform across hues in
  both modes.

Assign colours **stably by calendar ID hash**, not by load order, so a calendar's colour
never changes between sessions. Let the user override per-calendar.

---

## 7. Multi-account overlay — five accounts at once

This is the axis where the field is weakest and where Mach can be genuinely better.

### Colour by calendar, group by account

Every good client colours by *calendar*, not by account, because within one account you
still need to tell work-vs-personal apart. Account is expressed **structurally in the
sidebar** — a collapsible group header per account showing the email address — not by
hue.

Sidebar structure:

```
▼ you@example.com
    ☑ Alex Rivera
    ☑ Birthdays
▼ alex@northwind.example
    ☑ Work
    ☑ Team
```

### Fast toggling — the thing to actually get right

Toggling calendars must be instant and keyboard-reachable. Recommendation:

- **`⌘1`…`⌘9`** toggle the first nine calendars in sidebar order.
- **`⇧⌘1`…`⇧⌘5`** solo an *account* — show only that account's calendars, hide the rest.
  Press the same combo again to restore. This is the "focus one account" mode; nobody
  ships it well and it is exactly right for someone with five.
- Toggling must be **local-state instant** — never a round trip. Fetch everything, filter
  in the client.

### Duplicate events across accounts — the real win

When you're invited on one account and the meeting is copied to another, the same meeting
renders twice, side by side, halving the width of both. With five accounts this happens
constantly.

**No mainstream client solves this natively.** Google Calendar's own support threads on
duplicate events across accounts resolve to "turn off the duplicate calendar", and the
best-known fix is a third-party browser extension ("Event Merge for Google Calendar")
whose entire reason to exist is collapsing these. That is a clear gap.

**Implement merge-on-render:**

- Two events are the same meeting if they share an `iCalUID`, **or** (fallback) if start,
  end, and normalised title all match.
- Render **one** block. Colour it with the calendar of the account where you have
  actually responded, falling back to the first account in sidebar order.
- Show a small stacked-layers indicator (two offset rounded rects, 8px) in the block's
  top-right corner; the detail popover lists every account the event appears on.
- Make it a setting, defaulting **on**.

`iCalUID` is returned by the Google Calendar API on every event and is stable across
copies of the same meeting, so this is cheap and reliable. This alone will visibly
declutter a five-account week.

---

## 8. Known failure modes to avoid

Compiled from user complaints per app:

**Google Calendar**
- Overlapping events can be fully buried and unclickable — spawned fix-it extensions
  (GCalPlus) and long-running support threads asking for layering control. *Avoided by
  §2's equal-column choice + hover-to-expand.*
- The `+N more` popover in the all-day row covers the grid you're comparing against.
  *Avoided by expanding in place (§1).*
- Duplicate events across accounts, with no native merge. *Avoided by §7.*
- Keyboard shortcuts are **off by default** and buried in settings. *Avoided: on by
  default, `?` discoverable.*

**Notion Calendar**
- Shortcut list is in-app only and undiscoverable from the web.
- Post-acquisition the product has drifted toward Notion-database integration rather
  than calendar craft.
- Google-account-centric; multi-account handling is good but colour management across
  many calendars is still manual.

**Fantastical**
- Subscription pricing is the dominant complaint; feature creep into tasks, weather and
  contacts has made it heavier than the original.
- Fully modifier-chord keyboard model, slower than bare-letter.
- Its overlap-reading fix requires a two-modifier chord (Shift+Control+hover) which
  almost nobody discovers. *Avoided: plain hover in §2.*

**Vimcal**
- Expensive, and positioned at executives/EAs rather than individuals.
- No public shortcut documentation — a real discoverability gap for a product whose
  entire pitch is keyboard speed.

**Amie**
- See §9.

---

## 9. Amie is no longer a reference

Amie was widely praised for interaction design in 2022–2023 and is still cited that way
in older writeups. **It has since pivoted.** As of August 2026 amie.so leads with "Run
your workday on autopilot with AI agents" and describes itself as an AI note-taker
whose MCP gives Claude or ChatGPT access to meeting notes, calendar, email and todos.
Its own reviews page is titled "Amie - AI Note Taker".

Do not chase Amie's current product for calendar UX. The praised calendar craft — haptics,
motion, drag feel, todos-into-calendar — belongs to a version that is no longer the
company's focus. Product Hunt reviews (4.8 / 44) still praise polish and "making planning
feel pleasant", which is worth remembering as a *tone* target, but there is no current
artifact to copy specifics from.

The one durable lesson worth keeping: **motion is part of the product.** Drag, resize and
create should be spring-animated (~150–200ms, slight overshoot), not instant snaps.
That's cheap to do and is most of what people meant when they said Amie felt good.

---

## 10. Open source worth lifting

All licenses verified against the npm registry, August 2026:

| Package | Version | License | Use |
|---|---|---|---|
| `chrono-node` | 2.10.1 | **MIT** | **Adopt.** Natural-language date/time parsing (§3). |
| `react-big-calendar` | 1.20.0 | **MIT** | Reference only. Its `no-overlap` day-layout algorithm is the same one Mach already has. Read it to cross-check edge cases; don't take the dependency. |
| `@schedule-x/calendar` | 4.6.1 | **MIT** | Reference only. Modern, actively maintained; useful for theming ideas. Taking it would mean adopting its whole rendering model. |
| `@fullcalendar/core` | 7.0.2 | **MIT** | Reference only. Note FullCalendar sells premium plugins — the core is MIT but scheduler features are not. Avoid the ecosystem. |

**Recommendation: take exactly one dependency — `chrono-node`.** Everything else in this
brief is ~600 lines of code Mach already mostly has. Adopting a calendar framework would
mean inheriting its layout model, its theming system and its opinions, which is the
opposite of what this brief is for.

Cal.com is **AGPL-3.0** for the main application. Do not copy code from it into Mach
unless you are happy for Mach to be AGPL. Its booking grid is not separately licensed as
a reusable package. **Treat cal.com as look-don't-touch.**

---

## 11. Needs a taste call

These are genuine taste calls, not researchable facts:

1. **Density.** 48px/hour is Google's number and shows ~16 hours on a 13" laptop. A
   40px option would show all 24 with no scrolling but makes a 15-min event 10px.
   **Settled: 48 fixed.** Mach has one display and no density setting — the thread list
   had a compact/comfortable pair and it was removed rather than extended.

2. **Weekend columns.** Full-width equal columns for all 7 days, or narrow Sat/Sun to
   ~60% width to buy space for the working week? Notion Calendar hides weekends entirely
   behind a shortcut. Recommendation: equal width, with a toggle.

3. **Declined events.** Hide entirely, or show outlined-with-strikethrough? Google shows
   them by default and most people find it noisy. Recommendation: hide, with a toggle.

4. **The `n` key.** §4 resolves the Google-vs-Notion conflict in Google's favour (`n` =
   next date range, event navigation moves to `Tab`). If your reflex is actually
   Notion's, this flips.

5. **Merge-duplicates default.** §7 proposes defaulting **on**. It is the right default
   but it does hide the fact that a meeting exists on two calendars, which occasionally
   matters when deciding which account to reply from. Confirm.

6. **Vimcal's full shortcut map** could not be sourced publicly — it isn't in their docs
   site. Getting it would need a trial account. Worth chasing, or is Notion's map enough?
   Recommendation: enough.

7. **Colour palette.** §6 specifies a generated 8-hue OKLCH ramp rather than Google's 11
   named colours. If you want your existing Google colour assignments to carry over
   visually, we'd use Google's hex values instead and accept the uneven lightness.

---

## Appendix — measured Google Calendar reference values

Chrome, 100% zoom, 1508px window, August 2026. All values are CSS pixels.

```
Hour row pitch            48
Time gutter width         56   (label 43px wide, right-aligned)
Hour label type           11 / 16, weight 500, #444746
Day column width          156.5   (at 1508px window, 7 days)
Event block width         column − 13   (right gutter preserved for drag-create)
Event border-radius       6
Event z-index             5
15-min block height       11   (text line 15px, overflows block)
45-min block height       34
Event title type          12 / 15 weight 500  (11 / 15 when compressed)
Event time type           12 / 15 weight 400
Now-line                  2px solid #DB372D, today's column only
Now-dot                   12 × 12 circle #DB372D, offset −6px into gutter
Now z-index               507
All-day chip height       22   (row pitch 24)
All-day chip radius       6, padding 0 8
All-day chip type         12 / 20 weight 500
Accepted event fill       solid calendar colour, white text
Pending-invite fill       #FFFFFF, title #1F1F1F, time + border in calendar colour
Sample palette values     #039BE5 (Peacock), #0B8043 (Basil)
```

## Sources

- Google Calendar web app — direct DOM measurement, August 2026
- Google Calendar keyboard shortcuts — https://support.google.com/calendar/answer/37034
- Stack Overflow #11311410, "Visualization of calendar events. Algorithm to layout events
  with maximum width" — accepted answer, 73 votes
- Fantastical for Mac, Tips and Tricks — https://flexibits.com/fantastical/tips
- Notion Calendar keyboard shortcuts — https://www.notion.com/help/notion-calendar-keyboard-shortcuts
  (points to in-app `?`); transcribed list via
  https://shortcutposters.com/notion-calendar-mac-keyboard-shortcuts-poster/
- Vimcal docs — https://docs.vimcal.com/
- Amie — https://amie.so/ (current positioning), Product Hunt reviews
- npm registry — license verification for chrono-node, react-big-calendar, schedule-x,
  fullcalendar
