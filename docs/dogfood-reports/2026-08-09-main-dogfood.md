# Dogfood — 2026-08-09 — `main`

**Status: partial.** Interrupted twice by the tree being edited underneath it (see
Limits). What is here is measured, not impressions; what is missing is named.

**Findings 1, 2 and 3 were fixed later the same day. The fixes and the new
measurements are recorded under "Resolution" at the end.**

## Scope

`main` is the trunk, so there is no branch diff. Scoped instead to the five
commits made today, which is everything in the current UI. Driven against
`localhost:1420`, which renders `src/lib/fixtures.ts` — outside Tauri there is no
IPC, so no request can reach the owner's mail, calendar or Google. Nothing was
stubbed because nothing needed to be.

Browser automation: `agent-browser` connected over CDP to the existing headless
Chrome (`agent-browser connect 9333`) so no window is opened and no focus is
taken.

## Findings

### 1. There is almost no typographic hierarchy — calendar view

Measured across 149 leaf text nodes:

| font-size | count |
|---|---|
| 11px | 77 |
| 12px | 63 |
| 13px | 9 |

Two weights in use: 400 (84) and 500 (65).

The largest text anywhere in the calendar is **13px**, and 11px carries half the
interface. A date header, an event title and a footer hint are within 2px of each
other, so the eye has nothing to anchor on and everything reads as one texture.
The type scale in `globals.css` defines `text-micro/list/body/reading`, so the
tokens exist; the calendar simply uses the bottom two.

Cheapest fix with the most effect: give the day-column headers and the current
date real size, and let event titles sit above their times rather than beside
them at the same weight.

### 2. The spacing scale has an unofficial 6px step

Padding and gap values across ~1,200 elements:

| value | count |
|---|---|
| 4px | 118 |
| 8px | 114 |
| **6px** | **109** |
| 12px | 51 |
| 1px | 48 |
| 2px | 20 |
| 14px | 9 |
| 3px | 8 |
| 16px | 5 |

4 / 8 / 12 / 16 is a clean 4pt grid. **6px is used as often as 8px**, and 3px,
2px and 14px appear alongside it. Half-steps between grid values are what makes
a layout feel unresolved without anyone being able to point at why. Either adopt
a 2pt grid deliberately, or push 6 → 4 or 8 and 14 → 12 or 16.

### 3. Overlapping events truncate to nothing

Tuesday 1–2pm in the fixture week renders three concurrent events as
`Intervi staf…`, `OKR…`, `Wareh cost…`. At three columns the title is
unreadable and the app is showing the user a shape rather than information.
Google's answer is to keep the first word legible and drop the time; Fantastical
overlaps with offset rather than dividing the width.

### 4. `agent-browser errors` reports nothing usable

Every call returned rows of empty strings, including while the page was showing
a full-screen Vite build failure. The dev-server error was caught by
`scripts/webqa.ts reload`, which holds one CDP session across the navigation.
Worth knowing before trusting a clean `errors` result as evidence.

## Limits — why this is partial

1. **The dev server was serving a build error.** `src/lib/message-body.ts` did
   not parse (a backtick inside the CSS template literal), so the app was a Vite
   overlay rather than an app. This also reached the owner's running window,
   which loads its frontend from the same server. Fixed by the agent that caused
   it, ~100s.
2. **The tree is being edited by a live agent** working on message rendering,
   which is exactly the area a mail dogfood needs. Any finding there would have
   been about a half-finished state.
3. **The mail half is therefore untested here.** Thread rendering, the reading
   pane, drafts and the composer all wait on that agent landing.

## Not done

- Mail journeys end to end (list → thread → reply → draft)
- Dark theme pass
- Keyboard-only traversal of both modes
- Responsive behaviour at narrow widths
- The automated suite as a gate for this report (it was green at the last commit:
  1076 frontend, 654 Rust)

## Verdict

Not ready to sign off. Two concrete design defects worth fixing (hierarchy,
spacing grid) and one usability defect (overlapping events). The mail half is
unexamined, so absence of findings there is absence of evidence.

---

## Resolution — 2026-08-09

Findings 1, 2 and 3 are fixed. Measurements below were taken the same way as the
ones above: `getComputedStyle` over every rendered element under `#root`, leaf
text nodes for type, padding and gap values for spacing, deduplicated per
element. Driven headless against `localhost:1420` at 1440×900.

### 1. Type scale

`globals.css` gained a fifth step, `--text-title` (17px), and `cn()` in
`lib/utils.ts` was told about it.

| token | px | job |
|---|---|---|
| `text-micro` | 11 | times, counts, key hints, the hour gutter, group headers |
| `text-list` | 13 | rows, controls, sender names, calendar names |
| `text-body` | 14 | the line a surface is read for — a thread's subject |
| `text-reading` | 15 | prose, and the period a view is showing |
| `text-title` | 17 | one per view: the calendar's day number, the open subject |

The event grid keeps its own 12px, which lives in `calendar-geometry.ts` as a
measurement rather than as a step of the ramp.

Calendar, 141 leaf text nodes:

| font-size | before | after |
|---|---|---|
| 11px | 77 | 98 |
| 12px | 63 | 29 |
| 13px | 9 | 6 |
| 15px | — | 1 |
| **17px** | — | **7** |

Weights: 400×84 / 500×65 before; 400×93 / 500×12 / 600×36 after.

The largest type in the calendar went from 13px to 17px, and the ratio between
the largest and the smallest from 1.18× to 1.55×. 11px rose because event
*times* dropped a step to sit under their titles, which is where most of the new
contrast inside a block comes from. The count is not the thing that was wrong;
the spread was.

Mail, 270 leaf text nodes:

| font-size | before | after |
|---|---|---|
| 11px | 118 | 164 |
| 13px | 150 | 59 |
| 14px | 1 | 46 |
| 15px | 1 | — |
| **17px** | — | **1** |

The thread row's three lines were 13/13/13. They are now 13 sender, 14 subject,
11 preview, in the same 4.25rem row: `leading-tight` on 14 and 11 comes to the
same 49px, so no conversation left the screen.

### 2. Spacing grid

4pt — 4 / 8 / 12 / 16 / 20 / 24 — written down at the top of `globals.css` with
three exceptions: 1px hairlines, 2px inside a painted event block, and the
measured constants in `calendar-geometry.ts`.

Calendar, ~1,130 elements:

| value | before | after |
|---|---|---|
| 4px | 132 | 200 |
| 8px | 113 | 121 |
| 12px | 50 | 50 |
| 16px | 6 | 6 |
| 1px | 46 | 46 |
| **6px** | **109** | **12** |
| **10px** | **45** | **0** |
| **3px** | **8** | **0** |
| 2px | 20 | 19 |
| 14px | 9 | 9 |

Mail is the same shape: 6px 116 → 16, 10px 45 → 0, 3px 8 → 0.

What is left off the grid, by name:

- **2px ×19** — `EventBlock.tsx`, the vertical inset of a timed block. Documented
  exception; a 24px block holds one 15px line.
- **6px ×12 and 14px ×9** — `AccountRail.tsx` rows (`gap-1.5`, and a 14px indent
  set from a style attribute).
- **6px ×4** — `ComposerDock.tsx`.

Those three files belong to other agents and were left alone.

Where a half-step became a whole one: `gap-1.5` → `gap-1` when the two things are
one unit (an icon and its label, a name and its count), `gap-2` when they are two
units in a row; padding took the nearer 4pt step, preferring the smaller, so
6→4, 10→8 and 3→4.

### 3. Overlapping events

Two changes, from the two references.

**Google's**, in `blockPlan`: under 88px a block drops the time and the location
and spends every line on the title. A block's position in the grid already says
when it is, and the time costs a whole line stacked or ~30px inline. 88px is
under the 97px a full-width column has at a 1040px window, so narrowing the
window leaves ordinary blocks alone.

**Fantastical's**, in `clusterPlan` and `columnGeometry`: from three events up,
a cluster whose even share would fall under 76px stops dividing and cascades.
Each block is offset by `step` and runs to the right edge of the cluster, drawn
over the one before it, with a 1px hairline in its own ink down its left edge.
`step` is `(width − 76) / (columns − 1)`, floored at 18px.

At 1440px a week column has 155px of usable width:

| | before | after |
|---|---|---|
| 3 concurrent | 51px each, `Intervi staf…` `OKR…` `Wareh cost…` | 39 / 39 / 74px, `Interv…` `OKR draft 2 wal…` `Warehouse cost review` |
| 5 concurrent | 3 shown at 51px, 2 behind a `+2` chip | 5 shown, 20px strips and a 76px block on top |
| 15-minute | unchanged sliver, unreadable in a cluster | cascades like any other block |

A cascaded block's title is laid out at the full remaining width and then
covered, so selecting it — which raises it to `Z_EVENT_SELECTED` — reveals the
whole title with no reflow and no geometry change. Arrowing through a five-deep
cluster reads every one of them in turn. No event is removed from the grid to
achieve it, and `visibleColumns` now admits seven events at 1440px where it
admitted three.

One thing had to change underneath it. A past event faded by setting `opacity:
0.6` on the block, which is the same picture as washing the paint towards the
page until two blocks overlap — at which point a cascaded block showed its
neighbour's title through itself. `paintFor` now washes the fill, the ink and the
border and leaves `opacity` alone. That also fixes a second instance of the
defect `EventBlock.tsx` documents at length: the selection cursor is drawn
outside the block and cannot survive `opacity`, so until now it was rendered at
60% on every event earlier than now.

### Not regressed

- The selection cursor's luminance sandwich is untouched; `selectionShadow` and
  `selectionShadowChip` are byte-identical, and the cascade hairline is
  suppressed while a block is selected so it cannot nick the mark. Verified by
  screenshot on a cascaded block in both themes.
- Calendar colours: `calendarFill`, `calendarInk` and `inkOn` are untouched, and
  `paintFor(..., { past: false })` returns exactly what it returned before. A
  future week renders Google's own hexes (`#3F51B5`, `#0B8043`, `#8E24AA`).
- The thread row is still three lines in `--spacing-row`, unchanged at 4.25rem.

### Gate

`bunx vitest run`: **1151 passing, 46 files**. It was 1098 across 44 files when
this pass started; other agents were adding tests at the same time, so not all of
the difference is this work. `calendar-geometry.test.ts` and
`event-layout.test.ts` carry 56 between them, covering the cluster plan, the
cascade geometry at two, three, five and seven deep, the fifteen-minute case, and
the width-driven block plan.

`bunx tsc --noEmit` and `bun run build` clean.
