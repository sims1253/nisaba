# UI mocks — three design directions for the Nisaba workspace

Three static, self-contained HTML mocks of the editor workspace. Open any of them
in a browser; no build, no network, no fonts to fetch (system font stacks only,
matching the app's offline constraint).

| File | Direction | One-line pitch |
|------|-----------|----------------|
| `mock-a-galley-desk.html` | **Galley Desk** | The writing desk: review lives in the margin, all state in one status strip. |
| `mock-b-pressroom.html` | **Pressroom** | Focus-first: one dock at a time, ⌘K navigation, build output as a press log. |
| `mock-c-project-control.html` | **Project Control** | The lifecycle is the interface: pipeline status, triage-table review, instruments dock. |

All three render the **same project state** — same document (`main.typ`, the demo
paper), same three open review items (one insert, one comment, one orphaned
delete), same build `#41` from checkpoint `c128`, same collaborators — so the only
variable is the interface.

---

## Review of the current UI (`web/src/main.ts` + `styles.css`)

The current build gets the fundamentals right: the source ↔ page split is the
correct backbone for a compile-based writing tool; review marks keep one color
language (green insert / amber comment / red delete) across inline decorations,
popovers, and sidebar cards; double-click in the PDF jumps to source; diagnostics
are clickable and land on `file:line`; and there is real accessibility care
(skip link, ARIA roles, reduced-motion, forced-colors, print stylesheet).

What it does less well, mapped to purpose:

1. **Toolbar sprawl.** Nine controls of three different scopes share one wrapping
   toolbar: project scope (References, Share, Export), document scope (view
   projection, Track changes, Review), and pane scope (Compile). At 1680px they
   already wrap into two ragged rows. A writer edits text 95% of the session;
   the toolbar gives occasional actions equal permanent weight with the primary
   one, and nothing groups by *what the action acts on*.
2. **One fact, three surfaces.** The amber review banner, the `Review ③` badge,
   and the review sidebar all announce the same "3 open items". The banner adds
   a fourth control for track-changes state, which also exists in the toolbar
   and the sidebar. Redundant state displays drift out of sync and teach users
   not to trust any of them.
3. **The "outline" is not an outline.** It is a flat, numbered file list. The
   `01–05` numbers are ordinals with no meaning to the author; folders (derived
   from paths) are flattened into truncated suffixes; and there is no
   heading-level navigation *within* the open document — which is how writers
   actually move through a long text.
4. **Glanceable state is scattered across five places.** Save status (topbar),
   sync + cursor (editor footer), connection badge (floating over the preview),
   build label (preview toolbar), review summary (banner). Answering "is
   everything fine?" costs five glances at four corners of the screen.
5. **Presence is degraded to text.** The sync protocol carries a live roster
   with heartbeats, but the UI reduces it to "2 collaborators online" — no
   *who*, no *where*.
6. **Full workflows squeezed into a rename-sized modal.** Share, References,
   History (timeline + diff), and Export all open the same 385px `<dialog>`.
   History and References are standing workflows, not prompts; the modal
   caps their information density and hides the document behind them.
7. **Monotone material.** Every region is the same hairline-bordered box with
   4–6px radius; hierarchy is carried only by small color deltas. It is
   inoffensive and generic — fine for a first pass, silent about what matters.

None of this is broken; the bones are good. The mocks below each take a
different position on how to fix 1–7.

---

## Mock A — Galley Desk

**Position: the editor is a desk, review is marginalia, state is one strip.**

- **Navigator (left).** Two lists, both purposeful: *Files* as a real indented
  tree (folders exist because paths exist; the entrypoint carries an `ENTRY`
  tag instead of a lock icon), and *Sections* — the open document's heading
  outline with dot leaders and line numbers, which is the navigation writers
  actually use. Project meta (references, checkpoints, entrypoint) sits at the
  bottom as facts, not buttons.
- **Source (center).** Measure-constrained column with a **reserved margin**.
  Suggestions render as text (green underline = insert, red strike = delete),
  comments as an amber-marked span; each mark has a small flag in the gutter.
  Clicking a flag expands a **margin note** — the thread lives beside the line
  it annotates, like a proofreader's comment, never on top of the text. No
  banner, no sidebar, no popover: one review surface, spatially anchored.
- **Page (right).** The compiled artefact with running head and folio; the bar
  states page position and *which checkpoint the build came from* — provenance
  is a first-class fact in Nisaba, the UI should say it.
- **Status strip (bottom).** Sync, cursor position, word count, the four
  projections (Proposed/Baseline/Redline/Public) as a text switch, build
  health, Export, and the one primary action: Compile. Every glanceable fact
  in exactly one place.
- **Material:** warm paper, ink, hairline rules, squared edges, serif chrome
  with mono metadata. Nothing rounded, nothing floats except the margin note,
  which is a ruled marginalium with a hard offset shadow.

Trades: the margin column costs text width; at narrow windows it should
collapse flags into a single "N marks" gutter button. Review threads longer
than a few replies need an expansion path (thread opens into the margin
column full-height).

## Mock B — Pressroom

**Position: focus-first. One dock, one jump bar, one log.**

- **Tool rail (far left).** Seven destinations, one open at a time: Files,
  Sections, Review, History, References, Share, Export — each is a dock, never
  a modal. Closing the dock (⌘0) returns the full window to writing. This
  directly fixes the toolbar sprawl and the modal-cramping problems: actions
  act on scopes, so they live in their own dock with room to work.
- **Jump bar (top).** `❯ jump to file, section, reference, action… (⌘K)` —
  with a flat path-addressed project model, fuzzy jump is the fastest
  navigation the app can offer, and it scales to 500-file projects where trees
  stop working.
- **Review as keyboard triage.** The queue is a flat ruled list; selecting an
  item frames its span in the text and docks an action bar *under the line*:
  `ACCEPT ⌘1 · REJECT ⌘2 · COMMENT ⌘3`, then `⌘↓` advances. A reviewer can
  clear a queue without touching the mouse — that is the workflow reviewers
  actually repeat hundreds of times.
- **Press log (bottom drawer).** Build results as a docked log strip with
  filter tabs (Errors 0 / Warnings 1) and jump-to-line links, instead of a
  hidden list that only appears on failure. Compile status is chronology, and
  chronology belongs in a log.
- **Mode switch.** Source / Split / Page decides how much of the window the
  artefact gets; presence shows *who and where* ("CL SP · editing §results").
- **Material:** utilitarian ink rail, squared everything, mono micro-labels,
  one deep press-green reserved for the primary action and the selection
  frame.

Trades: the rail's icon-only targets need tooltips/labels for discoverability;
first-time users must learn that docks exist (mitigated by the ⌘K bar listing
actions too).

## Mock C — Project Control

**Position: a long-lived project is a process; the UI should show where it stands.**

- **Pipeline strip (under the header).** `WRITE › REVIEW 3 open › BUILD #41 ✓ ›
  CHECKPOINT c128 › EXPORT last 3d ago`. This is Nisaba's actual domain model —
  checkpoints, builds, artifacts — surfaced as a status line. The current step
  is lit; each step is a jump target. No other surface in any mock answers
  "where is this project?" this fast.
- **Members with roles.** The header stack shows all four members with their
  role tags (OW/AU/RV/RO) — roles govern what you can do, so they should be
  visible, not buried in the Share dialog.
- **Navigator + checkpoint timeline (left).** Files above; below, the recent
  checkpoints as a restorable timeline (`c128 … DIFF·RESTORE` on hover).
  History becomes a place you can go back to, not a modal you open.
- **Instrument dock (right, under the artefact).** Tabbed: `REVIEW 3 ·
  DIAGNOSTICS 1 · HISTORY · ARTIFACTS`. Review is a **dense triage table** —
  type / author+role / excerpt with `path:line · section` / age / inline
  actions — with checkbox bulk-select and filter chips. This is the format
  that survives 40 open items, where card lists drown.
- **Two-page spread preview.** Page pairs, because that is how the artefact
  will be read.
- **Material:** cool grays, cobalt accent, tabular numerals, sticky table
  headers, sticky tabs. An ops console for a document, still squared and
  hairline-free of decoration.

Trades: density is the point but also the risk — it is the most "expert" of
the three and the least calm for pure drafting; the answer is that the Write
step de-emphasizes the dock (tabs collapse to a slim bar) when review is
empty.

---

## What all three deliberately keep

- Source stays monospace and honest (it is Typst, not a rich-text illusion).
- Suggestions are rendered *as text* (underline/strike), never as chips —
  review state must read as the document it will become.
- Build state names its checkpoint; artifacts name their provenance.
- One accent color per design, spent only on primary action + selection.
- No card grids, no rounded-corner containers, no gradients, no dark mode as a
  personality.
