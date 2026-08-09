# Dogfood — 2026-08-09 — `main`

**Status: partial.** Interrupted twice by the tree being edited underneath it (see
Limits). What is here is measured, not impressions; what is missing is named.

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
