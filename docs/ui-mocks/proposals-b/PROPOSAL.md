# Proposal B — “Ink Desk”: a typography-led art direction for Nisaba

> Companion to [`docs/ui-design.md`](../../ui-design.md), which this proposal
> argues with — respectfully, and only on material. The workflow, the anatomy,
> the vocabulary, and the keyboard model are kept exactly as shipped; this is a
> change of *voice*, not of *behaviour*. A conservative refinement of the current
> paper-and-hairline direction exists in parallel (proposal A); this document is
> the bold one, and it names its own costs honestly (§5).
> Mockups: open [`index.html`](index.html) in a browser. Every file is
> self-contained (inline CSS, system font stacks only — the app's no-web-font,
> works-offline constraint is honoured by the proposal itself).

---

## 1. The direction in one paragraph

**The interface is the ink; the document is the light.** Every surface that is
apparatus — app bar, navigator, docks, drawer, status bar — is cast in one warm
bitumen (a very dark olive-charcoal, the colour of dried stylus ink, not a
neutral near-black), and the two surfaces that *are* the document — the source
column and the rendered page — remain paper, now the only light things on
screen, so the writing glows and the apparatus recedes: a 95%-writing interface
that is literally built around the text. The accent moves from institutional
steel blue to **lapis ultramarine** — the one prestige material of the scribal
world Nisaba is named for — spent exactly as sparingly as before: primary
action, selection, active navigation, focus. Typography is promoted from
supporting role to identity: the serif that today lives only inside the
rendered page becomes the chrome's *display* voice (brand wordmark, document
name, projects, empty states — never below 16px, never for controls), while the
mono micro-label and the sans control voice stay and sharpen. Density is
unchanged — boldness comes from figure-ground inversion and scale, not from
adding chrome. The **signature element** is the projects screen rebuilt as a
**table of contents** — serif project names, dot leaders, role tags, folio-like
timestamps: your body of work presented the way a document presents itself. A
small recurring motif, the **wedge** (cuneiform: “wedge-shaped”; the mark a
stylus makes), replaces dots and bars wherever the interface marks *active*:
the live-sync indicator, active tree rows, state glyphs. The status bar becomes
a **colophon** — the scribe's closing line that recorded who wrote, when, and
how many tablets: `Live · 3 here · Ln 83 · 4,812 words · build #41 · from c128`.

### Where it comes from (grounded in the subject, per the design skill)

The skill's first instruction is to draw the distinctive choices from the
subject's own world — its materials, instruments, artifacts, vernacular.
Nisaba's subject world is *the tablet house*: Nisaba is the Mesopotamian patron
of scribes; the artifact is the inscribed tablet; the instrument is the wedge
stylus; the prestige material is lapis; the closing convention is the colophon.
The current paper/hairline direction, whatever its merits, belongs to the
print-shop — a world dozens of writing apps already borrow. The tablet house is
*ours by name*, and nobody else can claim it.

### Skill principles → what they drove here

| Principle (frontend-design skill) | What it drove in this proposal |
|---|---|
| “The hero is a thesis” — open with the subject's most characteristic element, not a template | The projects screen **is** a table of contents; the workspace hero frames paper in ink |
| “Typography carries the personality” — deliberate pairing, memorable type | Serif promoted to a display voice at a real scale (40/21/17.5/16), paired against the existing mono metadata voice; identity without adding a webfont |
| Calibration warning — avoid the three default AI looks (cream+serif+terracotta; near-black+single acid accent; broadsheet hairlines) | The current direction is adjacent to defaults 1 and 3 (warm cream, hairline rules, serif accents). Ink Desk exits via *structure* — light-dominant composition on dark chrome (not a dark page), warm bitumen (not neutral near-black), lapis (not acid green/vermilion), and a serif that carries meaning (document-ness), not decoration |
| “Structure is information” — numbering/dividers must encode real meaning | The dot leader in the ToC encodes the same thing it encodes in a book (name → location); the wedge marks *live/active* only; the colophon's segments are the actual provenance facts |
| Concentrate boldness in one signature element; keep the rest disciplined | The ToC screen + the ink/lumen figure-ground carry the drama; every control, row, and chip stays quiet, squared, and same-density as today |
| Copy is design too | Empty states get the display voice and one instruction + one key (“Pick up where you left off — ⌘K”); the sign-in screen speaks in one question (“Whose desk is this?”) |
| Quality floor, met quietly | Focus rings are lapis on ink and lapis-deep on paper; `prefers-reduced-motion` noted per mock; contrast floor checked (§5) |

---

## 2. Design tokens — old → new

Same token *architecture* (surfaces / ink / rules / one accent / semantic /
type / metrics): the proposal is implementable as a re-skin of the existing
`:root` block plus the additions marked **new**. Two tokens are **renamed**
because their meaning inverts (`--chrome` → `--bitumen`); everything else keeps
its name and changes value.

| Token (old) | Old value | Token (new) | New value | Notes |
|---|---|---|---|---|
| `--paper` | `#fdfcf9` | `--paper` | `#fbfaf5` | Barely moves; it is the document |
| `--chrome` | `#f2f3f0` | **`--bitumen`** | `#262921` | Apparatus ground. Rename: “chrome” said light furniture, “bitumen” says ink |
| `--chrome-raised` | `#f8f8f6` | **`--bitumen-2`** | `#2e322a` | Rows/chips/inputs on ink |
| — | — | `--bitumen-3` | `#383c31` | Hover tier (new; replaces `#0000000d` overlays) |
| `--shell` | `#e9eae6` | `--shell` | `#3a3e33` | The ground behind the rendered page darkens so the page glows |
| `--overlay` | `#ffffff` | `--overlay` → `--paper` | `#fbfaf5` | Popovers/palette/modals become *paper slips* on the ink desk |
| `--ink` | `#1b1e1c` | `--carbon` | `#221f17` | Text on paper (rename: `--ink` is now the ground, so the text-on-paper token must not be called ink) |
| `--ink-soft` | `#3d443e` | `--carbon-soft` | `#55524a` | |
| `--muted` | `#6a706a` | `--paper-muted` | `#8a8776` | Muted *on paper* |
| — | — | **`--bone`** | `#e9e7da` | Text on ink (new) |
| — | — | `--bone-muted` / `--bone-faint` | `#aab0a0` / `#7e8372` | Secondary/tertiary on ink (new; `--faint` splits by ground) |
| `--rule` / `--rule-strong` | `#e2e3dd` / `#c8cac2` | `--prule` / `--prule-s` | `#e3e0d0` / `#c6c3ae` | Rules on paper (values ~unchanged) |
| — | — | `--irule` / `--irule-s` | `#41453a` / `#575c4c` | Rules on ink (new) |
| `--accent` | `#23508a` | **`--lapis`** | `#4a63d8` | On ink: fills, active states, primary button |
| — | — | `--lapis-bright` | `#98a7ff` | Lapis as *small text* on ink (needs the lift; see risks) |
| `--accent-strong` | `#1b3f6e` | `--lapis-deep` | `#2b3fae` | Lapis on paper: links, citations, selection |
| `--accent-soft` / `--accent-line` | `#e8eef6` / `#b9cbe1` | `--lapis-wash` / (line = 1px `--lapis` at 60%) | `#eaedfb` | Selection wash on paper; borders become solid lapis, not tinted |
| `--insert` / softs | `#2e7d4f` / `#e4efe8` | unchanged on paper + **`--insert-lume`** | `#62c28d` | Semantics keep their jobs; *lume* variants are the same hues brightened for ink ground (drawer, dock chips, status bar) |
| `--comment` / `--delete` (+ softs) | `#9a6f18` / `#a63a2e` | unchanged + `--comment-lume` `#d3a83f` / `--delete-lume` `#e2705f` | | |
| `--sans` | DM Sans stack | unchanged | | Control/body voice |
| `--mono` | DM Mono stack | unchanged | | Metadata voice (now light-on-ink) |
| `--serif` | Source Serif 4 stack | **`--display`** (same stack) | | Promoted to the chrome's display voice |
| — | — | `--appbar-h` | `46px` → `52px` | The wordmark earns two more points of height |
| `--radius` | `3px` | `2px` | | Squarer still; only the avatar stamp and the pill switch curve |

Semantic colour discipline is **kept and sharpened**: green/amber/red still mean
insert/comment-warn/delete-error and nothing else; lapis still means *you/here/
primary* and never a meaning; author identity stays a deterministic hue per
name. The new thing the ink ground buys: the *document's* semantics (marks,
diffs, diagnostics on paper) keep today's exact colours, while the *apparatus*
states (drawer severities, status health, review chips) use lume variants of
the same hues — one language, two registers.

---

## 3. Type system

| Role | Face | Sizes | Used for |
|---|---|---|---|
| Display | `--display` (Source Serif 4 → Charter → Cambria → Georgia) | 40 / 26 / 21 / 17.5 / 16 | Projects masthead & ToC names, document title in the doc bar, brand wordmark, empty states, sign-in slip, history/diff version words |
| Control | `--sans` | 10.5–13 | Buttons, inputs, dock prose, review rows (unchanged) |
| Metadata | `--mono` | 8.5–11.5, uppercase 0.16em tracking | Micro-labels, crumbs, numbers, chords, status facts, role tags (unchanged role; now chalk-on-ink) |
| Source | `--mono` (user-settable per Settings) | 12–24 | The editor — on paper, measure-limited at 74ch, exactly as today |
| Page | `--display` | rendered | The preview is a document; unchanged |

Rule: **the serif never appears below 16px and never on a control.** Display is
for moments (names, titles, first impressions), not for furniture.

---

## 4. View-by-view change list

The summary table first; details (what / why / where in code / migration) follow.

| View | Changes | Mockup |
|---|---|---|
| Projects screen | ToC rows with dot leaders; serif masthead + names; colophon foot | `01-projects.html` |
| App bar | Ink ground; serif wordmark; paper palette-slip; roster chips; tablet-stamp presence | `02-workspace.html` |
| Navigator | Ink ground; wedge active marker; lapis MAIN tag; bone type | `02-workspace.html` |
| Document pane | Paper unchanged; dark title bar with serif document name; marks/carets unchanged | `02`, `03` |
| Review (marks + popover + dock) | Marks unchanged on paper; popover = paper slip; dock rows/chips re-inked; lume chips | `03-review.html` |
| Preview pane | Dark ground, page glows; view switch lapis; provenance label | `02-workspace.html` |
| Build drawer | Ink list; selected tab is a light slip; jump-to-line; lume severities | `04-problems.html` |
| Status bar | Colophon treatment; wedge live dot; lapis Update-preview block | all |
| Palette | Paper slip over dimmed desk; lapis selection bar; footer hints | `05-palette.html` |
| Share dock | Ink rows; owner tag in amber; links on lapis-dark field | `06-share.html` |
| Settings dock | Ink rows; lapis segmented active; capturing chord pulses | `07-settings.html` |
| History dock | Timeline in ink; diff as paper inset; pin chips | `08-history.html` |
| References dock | Serif titles per entry; state wedges; export-block tally | `09-references.html` |
| Signed-out | One paper slip on the ink desk; one question, one action | `10-signed-out-empty.html` |
| Empty states | Display voice + one instruction + one key; ❦ fleuron | `10-signed-out-empty.html` |

### 4.1 Projects screen — *the signature element*

* **What:** the screen becomes a book's contents page. Masthead: serif “Projects”
  at 40px over a 2px bone rule, with a small italic subtitle (“or, the
  contents”) and the primary New-project action in lapis, right-aligned.
  Rows become **ToC entries**: serif project name (17.5px) → dotted leader →
  role tag (mono) → `edited 2 h ago` (mono, tabular). Screen foot gains a
  colophon line (product + refresh time). Hover raises the row on bitumen-2 and
  the name goes white; focus adds the lapis left bar. The delete × stays
  reveal-on-hover, exactly as now.
* **Why:** the skill's hero principle — the most characteristic thing about a
  writer's shelf of long-lived projects is that it *is* a body of work, and the
  honest idiom for a body of work is a table of contents. Also plain
  usability: rows with leaders scan at least as fast as plain rows, survive 60
  projects, and the leader gives the eye a track from name to metadata that the
  current flex row lacks.
* **Where:** `web/src/main.ts` `renderProjects()` (row template gains
  `toc-name` / `toc-leader` / `toc-role` / `toc-when` spans); `shell.ts`
  `.screen-head` (masthead); `styles.css` projects-screen block.
* **Migration:** `project-row` → `toc-row`; the search/sort tools are kept
  verbatim (re-skinned). The leader is a flex filler with a dotted bottom
  border, `min-width: 32px`, so long names degrade to ellipsis *before* the
  leader vanishes — tested in mock with the longest realistic name.

### 4.2 App bar

* **What:** ink ground (`--bitumen`, `--appbar-h: 52px`). Brand becomes the
  lapis square “N” plus the wordmark **Nisaba in the display serif**. The
  palette hint becomes the only light element in the bar — a paper slip
  (`--paper`, tiny under-shadow) so ⌘K reads as “a slip of paper on the desk,”
  the destination of the palette itself. Roster chips keep their structure
  (name + role) on ink. Presence avatars change shape from circles to
  **rounded-2px squares — tablet stamps** — with the same per-name hue, now
  generated darker with a bright keyline so they read on ink.
* **Why:** figure-ground concentration (skill: one signature, disciplined
  rest); the palette hint is the app's front door and deserves to be the one
  light thing in the apparatus (hero/thesis); presence is a first-class fact in
  Nisaba and stamps are more legible on ink than pale circles.
* **Where:** `shell.ts` `.brand` (wordmark span gains the display class),
  `.palette-hint`, `#project-people`, `#presence`; `styles.css` app-bar block;
  `main.ts` `renderPresence` (avatar shape is CSS-only; no logic change).
* **Migration:** `--chrome` → `--bitumen` in the bar; `.avatar` radius 50% →
  2px and the hsl backgrounds get a `box-shadow` keyline. Remote caret labels
  (in the text) keep their pill-on-caret geometry — unchanged.

### 4.3 Navigator (files + outline)

* **What:** ink ground; tree/outline rows in bone; the **active row carries a
  solid lapis left bar plus the row text in lapis-bright** (today: accent-soft
  background wash — the wash is unreadable on ink, so active becomes line +
  text, which also survives forced-colors better). The `MAIN` tag becomes a
  lapis outline chip. Indent guides stay dotted. The nav-foot facts keep their
  mono, now chalk. New secondary motif: the **active outline item's leading
  edge is a wedge** (`clip-path` triangle) — the stylus mark meaning “you are
  here.”
* **Why:** structure-is-information: active state was carried by a tint; on ink
  it must be carried by line + colour + (the wedge) shape, which is *more*
  information, not less. Keeps the doc's rejected-alternatives list intact
  (no numbering, no icons).
* **Where:** `styles.css` `.tree-row/.outline-row` rules only. Zero DOM
  change: the wedge is a `::before` on `.active`.

### 4.4 Document pane

* **What:** the pane body stays **paper** — the editor, its 74ch measure, the
  sticky heading, marks, diagnostics, remote carets: all unchanged tokens. The
  document bar becomes ink and its title becomes the **document's own name in
  the display serif (16px)** — the one word in the chrome that names the thing
  you are writing. Track-changes switch: pill stays, amber when on (lume on
  ink). Review button + count badge unchanged in structure, badge in amber lume.
* **Why:** the thesis — the text is the light; and the title-bar serif makes
  the *document*, not the tool, the loudest thing after the text itself
  (typography-led identity). Behaviour (one switch, count on the door) is
  exactly the documented design.
* **Where:** `shell.ts` `#document-name` (class change only); `styles.css`
  `.doc-bar`, `.switch`, `.count-badge`.
* **Migration:** `.doc-bar` background flips to bitumen; `.doc-id strong` gets
  `--display`. No structural change.

### 4.5 Review — marks, popover, dock

* **What:** marks **as text on paper keep today's exact colours** (green
  underline / red strike / amber ground) — untouched. The thread popover
  becomes a **paper slip**: paper body, 3px amber top edge, soft deep shadow —
  floating surfaces are slips of paper, the same rule as the palette. The
  review dock's rows re-ink; kind chips use dark-ground soft colours with lume
  text (e.g. `#1d3a2a`/`--insert-lume`); Accept/Reject become tinted-border
  buttons; the queue footer's key legend is unchanged. The selection Comment
  affordance keeps its amber border and gains the wedge glyph as its bullet.
* **Why:** collaboration visibility must *shine* in the new direction —
  keeping review marks byte-identical on paper while the dock goes ink makes
  the queue instantly findable and the document's own state unmistakably
  document. Slips-on-ink is one consistent rule for every floating surface
  (palette, popover, modals) instead of three ad-hoc surfaces.
* **Where:** `styles.css` `.review-popover*`, `.review-card*`, `.chips`,
  `.selection-comment-button`; `main.ts` unchanged (all class hooks exist).
* **Migration:** popover `--overlay` → `--paper`; chips `aria-pressed` style
  flips from `--ink` fill to `--bone` fill.

### 4.6 Preview pane

* **What:** the ground behind the page darkens (`--shell: #3a3e33`) so the
  paper page — with its existing shadow — genuinely glows; this is the
  direction's money-shot in daily use. View switch: active segment solid
  lapis. Build label unchanged in content (`build #41 · from c128 · 12:04`),
  now chalk mono. Failed/stale states state the truth plainly (“stale — build
  #42 failed” + last good build).
* **Why:** the preview is an artifact; artifacts are lit, not painted. Copy
  discipline per the skill (state the fact, name the provenance).
* **Where:** `styles.css` `.preview-pane`, `.view-switch`, `.build-label`;
  `main.ts` `renderViewSwitch`/build-label strings unchanged.

### 4.7 Build drawer

* **What:** drawer body goes ink; problems are mono rows with lume severity
  chips and a `jump to line ↵` affordance on the right. The **selected tab is
  a light slip out of the ink** (paper background, carbon text) — the same
  inverted-slip idea as the palette hint, applied to tabs. Problem-count badge
  in delete-lume on error.
* **Why:** problems are expert apparatus (ink), but the *selected* view is what
  you're reading now (light). One motif, applied twice, teaches the rule.
* **Where:** `styles.css` `.drawer*` rules only. DOM unchanged.

### 4.8 Status bar — *the colophon*

* **What:** same cells, same facts, same order; ink ground, mono chalk; the
  Live indicator becomes a **wedge** in insert-green (the stylus mark = you are
  inscribing live); the Update-preview primary action is a solid **lapis
  block** with its chord, full cell height, never hideable (kept).
* **Why:** the colophon is the honest ancestor of everything this bar already
  says (who, where, how many, from which build) — grounded, not decorative.
  The wedge gives the sync state a shape as well as a colour (colour-never-
  the-only-carrier, kept).
* **Where:** `shell.ts` footer markup unchanged except `#status-dot` may become
  a wedge span (CSS `clip-path`; keep the `data-state` contract); `styles.css`
  `.statusbar`, `#compile-button`.

### 4.9 Command palette

* **What:** the palette becomes the **paper slip over the dimmed desk**: paper
  body, carbon text, the query line with a lapis caret, selected item = lapis
  wash + 3px lapis left bar + lapis-deep label; groups in the mono micro voice;
  footer hints in paper-2 (`↑↓ choose · ↵ open · ⇥ cite · > commands only`).
* **Why:** the palette is the expert's front door; making it the same *slip*
  material as the app-bar hint visually connects the two entrances (click the
  slip / press ⌘K for a slip). Selection gains a bar because washes don't read
  on paper at speed.
* **Where:** `styles.css` `.palette*`; `main.ts`/`palette.ts` untouched.

### 4.10 Share / Settings / History / References docks

* **Share** (`06`): invite row on ink (inputs bitumen-2), role descriptions
  beneath in the hint voice; member rows with tablet stamps; the **Owner tag
  reads in amber** (it is the one role that can destroy — semantic reuse, not
  decoration); link codes sit on a lapis-dark field (`#232747`) so a “key”
  looks like a key.
* **Settings** (`07`): rows on ink; segmented typeface control's active segment
  solid lapis; sliders `accent-color: var(--lapis)`; a **capturing chord
  button pulses a lapis ring** (with `prefers-reduced-motion` fallback to a
  static ring); scope notes unchanged in wording.
* **History** (`08`): timeline in ink with checkpoint ids as secondary metadata
  (kept); selecting/diffing uses small **pin chips** (Viewing / Diff A / Diff B)
  so the two-selection mechanic is visible, not just cursor-afforded; **the
  diff pane is a paper inset in the dock** — a version is a document too, and
  added/removed keep today's green/red softs on that paper.
* **References** (`09`): entry titles in the display serif (they are *titles of
  works* — the serif means “document” wherever it appears); state glyphs
  (full-text ✓ / no-PDF ⚠) become wedges in insert-green/comment-amber; the
  export-blocking tally also surfaces in the status bar's build cell
  (`3 refs block export`) — the fact the nav-foot already states, said once
  more where the export decision is made.
* **Where:** `styles.css` dock blocks; `main.ts` `openShare/openSettings/
  openHistory/renderReferences` templates for the pin chips and tally line
  only. All flows, validations, and role gates untouched.

### 4.11 Signed-out & empty states

* **What:** signed-out screen: the ink desk, a **single paper slip** — eyebrow,
  display question (“Whose desk is this?”), one field, one lapis action, one
  line of context. Empty states (no document, no preview, no files, outline
  pending): display serif headline + one instruction + one key, with a ❦
  fleuron standing where an ornament belongs.
* **Why:** first impressions are the skill's other hero; the current empty
  states are correct but generic 14px sans. The serif fleuron is the only
  decorative glyph in the whole proposal, and it is spent exactly where there
  is nothing else to look at.
* **Where:** `shell.ts` `#editor-placeholder` / `#preview-placeholder`
  (headline class only); `main.ts` `renderProjects` empty branch,
  `renderFileTree` empty branch; a new signed-out state would land in
  `renderAuth()` (the mock proposes the destination, the auth flow is
  unchanged).

---

## 5. Risks & what this costs

Named plainly, including what this reopens:

1. **It reopens the spirit of “no dark theme” (ui-design.md §7).** The letter
   is not broken — this is *one* theme with dark *apparatus*; the document
   surfaces, rendered page, and PDF ground stay light, and there is no user
   toggle. But the owner deferred “dark surfaces” for cost, and this proposal
   reintroduces half of them. Cost honestly: every chrome token, every
   `#0000000d` hover overlay, the forced-colors and `prefers-contrast` blocks,
   the print stylesheet audit, and the PDF-viewer ground must be re-verified.
   The doc's own estimate — “the stylesheet is fully tokenised so it is a
   contained follow-up” — is why this is even proposable now.
2. **Near-black + single accent is a calibrated AI-default look.** The skill
   warns about exactly this silhouette. The mitigations are structural — warm
   olive bitumen rather than neutral black; light-dominant composition (the two
   paper panes carry most of the screen at 1440px); lapis, not acid green; the
   serif display voice and the ToC hero doing the personality work — but if
   executed lazily (neutral `#1e1e1e`, one saturated accent, no serif), this
   direction degenerates into “generic dark IDE.” The direction *depends* on
   its typography landing.
3. **The serif display voice rides system fonts.** No webfonts (offline, no
   phoning home — kept), so the display stack falls back Charter → Cambria →
   Georgia across platforms, with real metric variance. Discipline required:
   display sizes are fixed and roomy (≥16px, generous line-height), ToC rows
   ellipsize, no display text in buttons. If the owner later ships one bundled
   font, the display role is where it would go (a packaging decision, not a
   network one).
4. **Contrast floor needs engineering attention, not just design intent.**
   `--bone-muted` on bitumen ≈ 7:1 and `--bone-faint` ≈ 4.5:1 are fine; but
   lapis `#4a63d8` *as small text* on bitumen is marginal, which is why the
   token table introduces `--lapis-bright` for small text and reserves solid
   lapis for fills with white text. This must be checked per-usage in
   implementation (WCAG AA on every chrome pairing), and `prefers-contrast`
   will need its own ink variants.
5. **The ToC screen spends its charm once a week.** The workspace is where
   users live; its boldness is quieter (title serif, glowing page, colophon).
   If the owner wants daily drama, this proposal declines to provide it — the
   text is supposed to be the drama (the product's own 95% rule).
6. **Presence avatars change shape.** Circles are the industry convention;
   stamps are a statement. If it reads as novelty rather than identity, keep
   the stamp but soften to 4px radius — the direction does not live or die on
   this one element.
7. **Lume variants double the semantic palette.** Six semantic values instead
   of three (paper + ink registers of the same hues). The rule is mechanical
   (ink ground → lume), but it is a real addition to maintain.
8. **Deliberately *not* reopened (kept refused):** no mobile layout, no
   minimap/split editors/multi-file tabs, no pipeline strip, no dark *theme
   toggle*, no WYSIWYG toolbar, review state still lives in exactly two places,
   the primary action still lives in the status bar. Nothing in this proposal
   requires touching `keybindings.ts`, the sync protocol, or any role gate.

### Adoption path (if chosen)

1. **Tokens only** — swap the `:root` block, add bone/lume/display tokens,
   fix the resulting contrast failures. The app becomes Ink Desk with zero DOM
   changes except `.brand` wordmark and `.doc-id strong` display classes.
2. **The hero** — `renderProjects()` ToC rows + masthead.
3. **The slips** — popover/palette/modal → paper; drawer tab inversion.
4. **The motifs** — wedge active markers, stamp avatars, colophon status bar.
Each step ships independently and is revertible; step 1 alone delivers ~80% of
the visual change.

---

## 6. What is deliberately unchanged

The five-region anatomy, docks-not-modals, palette-driven interaction, the
vocabulary table, the keyboard model and rebindable chords, review state in two
places, `⌘⏎`/`⌘S` semantics, the measure limit, marks-as-text, semantic colour
discipline, per-tab restore, settings scopes, accessibility contracts (skip
link, ARIA, reduced-motion, forced-colors, print-the-artifact). Proposal B
argues the current *material and voice* are the generic part; the *bones* were
never the problem.
