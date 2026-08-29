# Proposal A — UI/UX review and refinement: a writer-first evolution

> **Brief:** refine the existing design language, not rebrand it. The shipped
> system ( [`../../ui-design.md`](../../ui-design.md) → `web/src/styles.css`,
> `web/src/shell.ts`, `web/src/main.ts`) is the baseline; this proposal reviews
> every view, keeps what the decision record gets right, and proposes small,
> load-bearing changes where writers still pay a cost. Every mockup in this
> directory uses the real token set and the real markup structure, so any change
> that is accepted lands as a diff, not a rewrite.

Open [`index.html`](index.html) and click through the six proposed views. All
mocks render the **same project state** as the original explorations: the
`Field lab` project, `main.typ` at `§Results`, build `#41` from version `c128`,
twelve pages, one warning, three open review items, collaborators
twinkleburst (you), sparkletoes, clawson, moonmoss.

---

## 1. Method: the skill principles that drove this review

This review follows the *frontend-design* skill (anthropics/skills). Its
principles, and how each one shaped a concrete change here:

| Skill principle | How it is applied in this proposal |
|---|---|
| **"The hero is a thesis"** — open with the most characteristic thing in the subject's world; a big label + stats is "the template answer" | For a writer, the most characteristic thing is *the manuscript they were last inside*. The projects screen opens with a **Continue-writing hero** (P1), not a sorted list under a heading. In the workspace, the hero is unchanged and correct: the text column. |
| **"Typography carries the personality of the page"** — deliberate display/body pairing, type as a memorable part of the design | The app already owns a document voice: Source Serif 4 is what every rendered page speaks. This proposal spends that voice in exactly **three** chrome moments — the brand wordmark, the landing hero's project title, the signed-out statement (G1) — and nowhere else. No new typeface is introduced. |
| **"Structure is information"** — numbering, labels, dividers must encode something true | The dock header becomes a **tool switcher** (K1): the tabs state what can be docked, which is real structure, where the current bare title states nothing. Tree indent guides are reused for the outline (G3) so depth means the same thing in both lists. A hairline divider in the app bar separates the people group from the document utilities (A2). |
| **"Spend your boldness in one place"** / Chanel's "remove one accessory" | One signature element (the hero). One decorative move (serif voice). And two accessories *removed*: the demo-document button leaves the Files header (N1), and the filled blue Accept button leaves every unselected review row (K3). |
| **Motion: an orchestrated moment, not scattered effects** | No new motion is proposed. The existing micro-transitions (`background .12s ease` on buttons) stay. The one choreographed moment remains the drawer opening itself on build failure — a *state* change, not decoration. |
| **"Copy is design material"** — failures and empty states give direction; vocabulary is the signposting | Preview placeholder gains the `⌘↵` chord (V2); the first-run empty state becomes an invitation with two paths (P5); the signed-out screen says what Nisaba is and what to do next (U1). The build label's new staleness suffix is plain words: "edited since" (V1). |
| **AI-default critique** — warm-cream + serif + terracotta, near-black + acid accent, and broadsheet hairline grids are flagged as reflex defaults; "the brief's own words always win" | The current system is close to the broadsheet default (paper, hairlines, squared edges). This proposal does **not** run from it — the brief is a refinement brief, and the material system is the product's own decision record (§5 of ui-design.md). It differentiates deliberately instead: cool chrome vs warm paper (two materials, not one), a blue accent that never collides with review semantics, and the layer no template has — **people**: author-hue avatars, roster chips, remote carets. Presence is leaned into as the identity (G2), not decorated around. |
| **Quality floor** — responsive, visible focus, reduced motion | Every proposed control is a native button/select/input with the existing `:focus-visible` ring; nothing adds motion; all CSS is browser-shipped. |

---

## 2. Summary table

| View | Proposed changes | Mockup |
|---|---|---|
| Projects landing | Continue-writing hero (signature); role tags restored on rows; presence mini-stack on the hero; first-run invitation with demo path | [`a1-projects-landing.html`](a1-projects-landing.html) |
| App bar | Serif wordmark; roster chips get avatars + 3-chip cap with "+N" → Share; divider between people group and doc tools; presence avatars 24 px with "you" ring | [`a2-workspace-writing.html`](a2-workspace-writing.html) |
| Navigator | Demo button moves to empty state + palette; outline reuses tree indent guides; fact footer as aligned label/value grid | a2 |
| Document pane | Deliberately kept almost whole (74ch measure, mono source, sticky heading, remote carets); hairline separators in the doc bar; review popover gains a location line | a2, [`a3-workspace-review.html`](a3-workspace-review.html) |
| Dock tools (one language) | Header becomes a six-tool switcher; every dock opens with a one-line summary; review rows get progressive emphasis; Settings grouped under section heads; Share members get avatar chips | a3, [`a4-dock-family.html`](a4-dock-family.html) |
| Preview pane | Build label gains an amber "edited since" staleness suffix; placeholder gains the `⌘↵` chord; view switch, zoom, provenance tooltip kept | a2 |
| Build drawer | Problems sorted errors-first; everything else kept | a3 |
| Status bar | Selection word count ("4 of 4,812 words"); everything else kept | a2 |
| Palette | Structure kept; gains the relocated demo command | [`a5-palette.html`](a5-palette.html) |
| Review popover | Location line for parity with queue rows; amber rule, 272 px, actions kept | a3 |
| Auth / signed out | A stated welcome: serif sentence, primary Sign in, honest "projects appear after sign-in" | [`a6-states.html`](a6-states.html) |
| Empty states | First-run, no-files, no-preview all directional (say what to do, offer the chord or the demo) | a6 |

---

## 3. Change list, view by view

Each change: **what**, **why**, **where it lands** (file + region). No change
touches behaviour that the decision record locks (docks-not-modals, palette
reachability, one-switch track changes, review state in exactly two places,
`⌘⏎`/`⌘S` semantics, browser-chord refusals).

### G. Global

**G1. The document voice, three places only.**
*Source Serif 4* is what every compiled page speaks; it is the app's own
material, not an imported aesthetic. Use it for the brand wordmark
("Nisaba", small-caps, tracked), the landing hero's project title, and the
signed-out statement — and nowhere else in chrome. Body UI stays DM Sans;
metadata stays DM Mono. This is the proposal's one typographic personality
move, and it is drawn from the subject (its artefacts).
Lands: `web/src/styles.css` `.brand`, new `.screen-hero`, new `.signedout`.
*Principle: typography carries personality; ground it in the subject's world.*

**G2. One people-language everywhere.**
Avatar-initial chips on the deterministic author hue exist today in the
presence row and review rows. Extend the same chip to the app-bar roster
(`renderPeopleStrip`) and the Share members list, so "who" reads identically in
every region: same circle, same hue derivation, same initials. Presence is the
product's live layer and its most distinctive asset; unify it rather than
decorate it.
Lands: `styles.css` `.person`, `main.ts` `renderPeopleStrip`, `openShare`.

**G3. One depth language.**
The file tree renders nesting with dotted indent guides (`.tree-item .indent`).
The outline renders nesting with padding only. Reuse the dotted guide for
outline levels 2+ so depth looks like the same idea in both lists.
Lands: `styles.css` `.outline-row`.

### A. App bar

**A1. Wordmark in the serif voice** (G1). The blue `N` mark is kept; the
wordmark switches from sans to serif small-caps. Cost: one rule.
Lands: `styles.css` `.brand`.

**A2. Roster: avatars, cap, and a divider that states the grouping.**
Chips become avatar + name + role (G2), are capped at three visible, and
overflow becomes a "+N" chip that opens the Share dock — the roster is standing
information (decision record §4.1) but four-plus full chips crowd the bar at
1320 px, which is exactly where the palette hint sheds its label. Share stays
glued to the roster (they are one concern: people), and a 1 px divider
separates that group from References/History/Export (document utilities), so
the bar reads as *place · search · people · tools · state* instead of one long
row of equals.
Lands: `styles.css` `.people`, `.appbar-tools`; `main.ts` `renderPeopleStrip`.
*Principle: structure is information (the divider encodes the grouping the
design doc already argues for); remove an accessory.*

**A3. Presence: 24 px avatars and a "you" ring.** Peers keep their hover
location ("sparkletoes — main.typ · §Results"). You appear as the last avatar
with an accent ring, answering "which one is me" without a tooltip. No new
surface: this is the beloved existing row, made one step clearer.
Lands: `styles.css` `.avatar`, new `.avatar.you`; `main.ts` `renderPresence`.

**A4. Save status: kept as is.** *Saved 12:04 / Saving… / Unsaved changes* is
plain and honest; adding glyphs would decorate a fact that already reads.

### P. Projects landing screen

**P1. Continue-writing hero (the signature element).**
The screen opens with the most recently touched project as one large clickable
block: eyebrow "CONTINUE WRITING", project title in the serif voice, the
last-opened file and section ("main.typ · §Results"), "edited 2 h ago", the
live presence avatars of anyone inside it right now, and an explicit Open
button. Below it, the complete list (search + Recent/Name sort) is unchanged.
Why: the decision record's own goal for this screen is "get back to what I was
writing, in one click or none" — today that click is row one of a sorted list,
visually identical to every other row. The hero is that goal made visible. It
is a shortcut, not a second statement of state: the rows remain the complete
index. Data already exists (`nisaba.lastOpen` per tab + the API's recent
order + the presence roster for open projects).
Lands: `main.ts` `renderProjects` (renders a `#continue-hero` block above
`#project-tools`), `shell.ts` static container, `styles.css` new
`.screen-hero`. *Principle: the hero is a thesis.*

**P2. Role tag restored to every row.**
The decision record specifies rows "with name, role badge, and last-opened
time"; the implementation ships name + time only. Roles govern what a writer
can do before they open the project (a reviewer arrives to triage, an author to
draft) — restore the `.role-tag` chip. *Dependency:* `listProjects` must expose
the caller's role (or the client folds in a membership lookup); noted as an API
follow-up, the UI change is one span per row.
Lands: `main.ts` `renderProjects`; API: projects list response.

**P3. Search, sort, count note: kept.** The tools row and result counting are
correct and already ship.

**P4. First-run empty state = invitation with two paths.**
"No projects yet" gains its second path: *Create your first project* (primary)
and *Add the demo document* (secondary, absorbing the navigator's ✧ button —
see N1). An empty screen is an invitation to act, and for a self-hosted
product the demo is the fastest first act.
Lands: `main.ts` `renderProjects` empty branch.

### N. Navigator

**N1. The demo button leaves the Files header.**
Three permanent icon buttons (`＋ ✧ ‹`) give an occasional action (add the demo
document) equal weight with the daily ones (new file, hide sidebar). The demo
moves to the two places it is actually wanted: the first-run empty state (P4)
and a palette command. The header keeps two buttons.
Lands: `shell.ts` `nav-head-actions`; `main.ts` palette items.
*Principle: remove one accessory; the 1% must not occupy permanent space — the
decision record's own rule, applied to its own chrome.*

**N2. Outline indent guides** (G3).
Lands: `styles.css` `.outline-row[data-level]`.

**N3. Fact footer as an aligned grid.** "files 6 / references 3 of 12 with a
PDF / preview builds main.typ" becomes a two-column label/value list with
tabular numerals — the same facts, aligned so the numbers scan in a column.
Still facts, not buttons.
Lands: `styles.css` `.nav-foot`; `main.ts` `renderProjectFacts`.

**N4. Kept:** the real tree (folders from paths, `MAIN` tag), the live outline
with line numbers, the active-row treatment, equal independent scroll regions.

### D. Document pane

**D1. Kept almost whole, deliberately.** The 74ch measure, mono-and-honest
source, in-place Typst styling, sticky heading, track-changes switch, remote
carets with name labels, selection-comment affordance: this is the 95% surface
and it is right. Calm here *is* the design.

**D2. Doc-bar separators.** Hairline dividers between revision · switch ·
Review so the bar's right side reads as three groups rather than one ribbon.
One-rule change.
Lands: `styles.css` `.doc-bar-right`.

**D3. Review popover gains its location.** The popover shows author, age, kind,
text, actions — but not *where*, which every queue row states. Add the
`main.typ · line 31` line for parity, so the thread is anchored even when the
text scrolled.
Lands: `main.ts` `openReviewPopover`.

### K. Dock tools — one design language

**K1. The dock header becomes the dock switcher.**
Today the dock is a dead end: to move from History to Share you close, aim at
the app bar, reopen. Replace the bare mono title with a six-tab strip —
`REVIEW 3 · REFERENCES · HISTORY · SHARE · EXPORT · SETTINGS` — active tab
underlined in accent, close button right, at 9 px mono it fits the 340 px dock.
This is *navigation within the dock*, not a second door: the app-bar buttons
and the palette keep working, one dock stays open at a time, and the strip
states what can be docked (structure is information).
Lands: `shell.ts` `.dock .pane-bar`; `main.ts` `showPanel`/`syncDockButtons`;
`styles.css` new `.dock-tabs`.

**K2. Every dock opens with a one-line summary.**
Review: "3 open — 2 from other people". References: "12 entries · 3 with
PDFs". History: "working copy v128". Share: "4 people". Export: "Final · PDF
+ cited files". The room states its contents at the door; writers orient before
they read.
Lands: each `open*` function in `main.ts`; `styles.css` `.dock-summary`.

**K3. Review rows: progressive emphasis.**
Today every row ends in a filled accent Accept button — at 40 open items the
queue is a wall of blue and the primary color stops meaning "primary". Unfocused
rows get quiet text actions (green Accept, muted Reject); the selected row gets
the filled primary Accept. Keyboard triage (↑↓/A/R/C) is unchanged and remains
the fast path; the mouse path gets emphasis exactly where attention is.
Lands: `styles.css` `.review-card-actions`.
*Principle: spend emphasis in one place per surface.*

**K4. Settings dock: three sections, stated.** The settings stack
(opening-file, typography, keyboard) becomes three micro-headed sections —
*Opening file · Editor typography · Keyboard* — using the existing dock `h3`
style, with the reset notes kept per section. Scope before control.
Lands: `main.ts` `openSettings` markup.

**K5. Share: members as people, not strings.** Member rows gain the avatar chip
(G2) before name + role; Remove appears on hover. The invite control, role
descriptions, and links list are kept — the role vocabulary is already right.
Lands: `main.ts` `openShare`.

### V. Preview pane

**V1. The build label states staleness.**
Provenance is first-class (§4.5): the label says which view and when it was
built. The missing half of provenance is *whether it is current* — today a
writer compares the save time in the header with the build time here. After any
edit that outlives a debounce, append an amber `· edited since` suffix (tooltip
unchanged: build id, duration). Plain words, one colour with a job, removed the
moment the build lands.
Lands: `web/src/compile.ts` `renderBuildLabel`; `styles.css` `.build-label`.

**V2. Placeholder gains the chord.** "Open a document and choose Update
preview" + `⌘↵` kbd, mirroring the editor placeholder's `⌘K`. Both empty states
then teach the same lesson: the keyboard is the escape hatch.
Lands: `shell.ts` `#preview-placeholder`.

**V3. Kept:** the view switch on the page's own bar, the four writer-words
(Final · Original · All markup · Public copy), zoom scoped to the pointer,
page-position, provenance in the tooltip, the paper-on-shell ground.

### B. Build drawer

**B1. Problems sorted errors-first.** Errors block the preview; warnings do
not. Order the list by severity so the blocking item is row one.
Lands: `main.ts` `renderDiagnostics`.

**B2. Kept:** tabs, self-opening on failure, click-to-span, the log as a
chronological expert surface, the 190 px strip.

### T. Status bar

**T1. Selection word count.** While a selection is active the word cell reads
"4 of 4,812 words"; collapse the selection and it returns to the total.
Writers measure sections against targets; this costs zero chrome.
Lands: `main.ts` selection listener near `refreshDocumentStructure`.

**T2. Kept — whole.** One bar owning every glanceable fact, sync honesty (the
browser's offline event dims it immediately), the never-hideable primary action
beside the health it produces. This is the decision record's best invention.

### C. Palette

**C1. Kept in structure** (combobox, groups, `>` for commands). It gains the
relocated "Add the demo document" command (N1) — the palette is where
occasional actions go; that is its job.

### U. Auth / signed out

**U1. A stated welcome.** Signed out, the projects screen shows the serif
statement — "A quiet place to write documents together." — one line of what
Nisaba is, a primary Sign in button, and the honest note that projects appear
after sign-in. Not a dead end and not a fake demo: a direction.
Lands: `main.ts` `renderProjects` signed-out branch, `renderAuth`;
`styles.css` `.signedout`. *Principle: copy is design material; empty states
give direction.*

---

## 4. What we deliberately kept

The current design gets a great deal right; this proposal defends it explicitly:

- **The five-region anatomy and its scopes** (header = project, doc bar =
  document, preview bar = view, dock = one standing workflow, status bar =
  glanceable facts). Every change here lives inside a region; none adds a
  region or a floating layer.
- **The 74ch measure and the centred text column.** The single biggest
  legibility win in the app; untouched.
- **Docks, not modals** — and the modal kept for one-question prompts.
- **Review state in exactly two places** (the button's count and the queue).
  The dock tab count (K1) *is* the queue's count; no third surface returns.
- **Suggestions rendered as text** — underline, strike, amber ground — never
  chips.
- **The writer's vocabulary** (Update preview, Problems, version, needs
  re-anchoring, Final/Original/All markup/Public copy). No label changes.
- **One accent, semantics never decorative** — the new "edited since" suffix
  uses the existing warning amber for an existing meaning (something needs
  attention); no new colours anywhere.
- **The keyboard model** — palette-first, rebindable chords, browser-chord
  refusals, review triage bound only inside the dock.
- **Accessibility floor** — skip link, focus rings, ARIA listbox/combobox,
  reduced-motion, forced-colours, print stylesheet. All proposed controls are
  native elements inside that floor.
- **Presence, cursors, roster** — the beloved collaboration layer is the
  identity of this product; this proposal amplifies it (G2, A2, A3, P1) and
  adds no competing visual noise.

## 5. Feasibility notes

- Every change is native HTML/CSS/TS against the existing token set; no new
  dependencies, no web fonts, nothing offline-hostile.
- Two changes lean on small data additions, flagged rather than hidden:
  role-on-project-rows (P2) needs the role in the projects list response;
  the hero's presence mini-stack (P1) reads the roster already published by the
  sync relay for open projects, and simply omits it when the project is closed.
- Suggested landing order (each independently shippable): V1 + V2 → K1 + K2 →
  A2 + A3 → P1 → K3 → N1 + P4 → the rest.
