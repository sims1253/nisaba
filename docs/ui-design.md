# Nisaba UI — design rationale

This document explains the reworked workspace: who it is for, what each flow needs, and
why each surface looks the way it does. It is the spec `web/src/` implements. The three
explorations in [`ui-mocks/`](ui-mocks/) remain as the source material; this is the
synthesis that shipped.

---

## 1. Who this is for

Nisaba is a self-hostable collaborative authoring platform for long-lived document
projects. The people in front of it are **writers**, not developers: researchers, analysts,
policy and report writers, editors. They arrive from Word, Google Docs, and Overleaf. They
know track changes, comments, a file list, and a page preview. They do **not** know build
pipelines, diagnostics panes, or dotfiles.

That produces the governing rule of this rework:

> **Everything a writer does daily must be obvious without instruction. Everything an
> expert does occasionally must be reachable without cluttering the daily surface.**

The escape hatch for the second half is the keyboard — the command palette (`⌘K`), not
more buttons. This is the one thing modern code editors (VS Code, Sublime, Zed) get
right that office suites do not, and it transfers to prose without importing any of
their developer aesthetics.

A typical session is 95% writing, 4% reading the rendered page, 1% everything else
(references, sharing, history, export). The interface is weighted accordingly: the
1% must never occupy permanent screen space.

### Vocabulary: what the domain calls it vs. what the UI says

The platform's model is precise and stays precise in the code, the API, and the docs.
The interface speaks the writer's language, with the technical term available as
secondary metadata (tooltips, the history and build surfaces).

| Domain term | UI label | Why |
|---|---|---|
| compile | **Update preview** | Writers do not compile. They want the page to catch up. The keyboard hint `⌘⏎` is on the button; the tooltip still says "compile". |
| diagnostics | **Problems** | Plain word for "the thing that is wrong on line 12". |
| build / build id | **Preview** / shown as `#41` in the build drawer only | Build identity matters for provenance, not for drafting. |
| checkpoint | **version** (History), `c128` shown as metadata | "Restore version from 12:04" reads; "restore checkpoint c128" does not. |
| entrypoint | **MAIN** tag on the file | One word, in the file tree, where the question is asked. |
| projection: proposed | **Final** | Every suggestion applied — what the document becomes. |
| projection: baseline | **Original** | Every suggestion rejected — the last agreed text. |
| projection: redline | **All markup** | Word's own term for the marked-up rendering. |
| projection: public | **Public copy** | Final, minus redacted spans. |
| suggestion (insert/delete) | **suggested change** | Track-changes vocabulary. |
| mark / anchor | never shown | Implementation detail. |
| orphaned anchor | **needs re-anchoring** | Says what to do, not what broke. |
| CRDT / sync relay | **Live / Local** in the status bar | The writer needs to know whether peers see them, nothing more. |

---

## 2. Starting point: Overleaf, then the mocks

Overleaf is the default because it is the tool this audience already knows and because
its backbone is correct for a compile-based writing tool:

* files on the left, source in the middle, rendered page on the right;
* one primary action that refreshes the page;
* review threads docked beside the text, never on top of it;
* `⌘⏎` compiles, click-through between page and source.

That backbone is kept. What is reworked is everything Overleaf does *not* have to solve
and the current Nisaba client solved badly. The mock review ([`ui-mocks/README.md`](ui-mocks/README.md))
found seven problems; each is resolved below, with the mock that supplied the idea.

| # | Problem in the old client | Resolution | From |
|---|---|---|---|
| 1 | Nine toolbar controls of three different scopes in one wrapping row | Controls are grouped by **what they act on**: project actions in the header, document state in the document bar, view state in the preview bar, glanceable facts + the primary action in one status bar | A, B |
| 2 | The same "3 open items" announced by a banner, a badge, and a sidebar | The banner is deleted. Review state exists in exactly two places: the count on the Review button (the *door*) and the panel itself (the *room*) | B |
| 3 | The "outline" was a flat, numbered file list | Two real navigators: a **file tree** with folders derived from paths, and a live **section outline** of the open document's headings | A |
| 4 | Save, sync, cursor, connection, build, review scattered over five corners | One **status bar** owns every glanceable fact, plus save state in the header where it has always been | A |
| 5 | Presence reduced to "2 collaborators online" | Real presence: avatars with names and **where each person is** (`main.typ · §Results`), driven by the sync protocol's roster, which the client previously never read | A, C |
| 6 | Share, References, History, Export crammed into a 385 px modal | A right-hand **dock** hosts standing workflows at full height; the modal is reserved for one-question prompts | B |
| 7 | Every region an identical hairline box; hierarchy carried by 4% colour deltas | A material system: warm paper for content, cool neutral for chrome, one reserved accent, semantic colour used only for meaning | A, B, C |

---

## 3. Anatomy

```
┌────────────────────────────────────────────────────────────────────────────────┐
│ N  Projects › Field lab › main.typ › §Results   ⌘K   ⬤⬤⬤  Saved 12:04   ⌄ me  │ header
├─────────────┬────────────────────────────────┬─────────────┬───────────────────┤
│ FILES     ＋│ main.typ · v128   Track changes│ REVIEW    ×│ Final ▾  p 1/12   │
│ ▾ chapters/ │ ─────────────────────────────  │ ─────────── │ build #41 · c128  │
│   intro.typ │  == Results            (sticky)│ All 3       │ ┌───────────────┐ │
│   main.typ ⬤│                                │ ┌─────────┐ │ │               │ │
│ OUTLINE     │  Honey badgers attended 74% of │ │+ insert │ │ │     page      │ │
│  Abstract   │  recorded rave events…         │ │ "…bass" │ │ │               │ │
│  Results  ▸ │                                │ │ [✓] [✗] │ │ │               │ │
│             │                                │ └─────────┘ │ └───────────────┘ │
├─────────────┴────────────────────────────────┴─────────────┴───────────────────┤
│ PROBLEMS 1 · LOG          12:04:31  ok  build 41 — 12 pages, 1 warning         │ drawer
├────────────────────────────────────────────────────────────────────────────────┤
│ ⬤ Live · 3 here   Ln 12, Col 47   4,812 words        ✓ 1 warning  Update preview│ status
└────────────────────────────────────────────────────────────────────────────────┘
```

Five persistent regions, each with exactly one job:

1. **Header** — where you are (breadcrumb), who is here (presence), whether your work is
   safe (save state), and the project-scope actions.
2. **Navigator** — where things are: files, then the sections of the open document.
3. **Document** — the text, with its own identity bar (name, version, track-changes).
4. **Dock** — one standing workflow at a time: Review, References, History, Share, Export.
5. **Preview** — the artefact, with the view switch and its provenance.

Plus two conditional regions: the **build drawer** (problems and log; opens itself when a
build fails) and the **status bar** (always present; every glanceable fact and the primary
action).

The project list is **its own screen**, not a mode of the navigator. Previously the left
column meant "projects" sometimes and "documents in this project" other times, while an
empty editor and empty preview filled 80% of the window. Now: pick a project on a screen
built for picking projects; the workspace only exists once there is something to work on.

---

## 4. Flow by flow

Each flow states what the writer is trying to do, the decision, and what was rejected.

### 4.1 Arrive and open something

*Goal: get back to what I was writing, in one click or none.*

* The projects screen lists projects as rows with name, role badge, and last-opened time —
  not cards. Rows scan faster and survive 60 projects.
* Above the rows: a search box (name filter as you type) and a Recent/Name sort
  toggle. Recent (default) is the API's order — most recently *touched* first,
  where document create/update/delete counts as touching, not just renames.
* While a project is open, the app bar shows the who-has-access roster as name
  + role chips next to Share — membership is standing information for every
  member, not something only managers should find inside the Share dock.
* The last project + document reopen automatically (this already existed; it now lands in
  a workspace that looks the same as when you left it). Reopening is **per tab**: each
  tab returns to the project *it* had open (sessionStorage); only a brand-new tab falls
  back to the most recently used project anywhere (localStorage) — so two tabs on two
  projects stay on their own projects across reloads.
* `⌘K` from anywhere opens the palette; typing a file name and pressing Enter opens it.

*Rejected:* keeping the project list in the left rail. One column cannot mean two things,
and the empty three-pane workspace behind it was noise.

### 4.2 Find your way around a document

*Goal: jump to the part I am working on, in a 60-page document.*

* **Files** is a real tree. Folders come from the paths (`chapters/intro.typ` nests), which
  is the platform's actual model — the old flat list with truncated path suffixes hid it.
  The entrypoint carries a `MAIN` tag; that is the file the preview builds from.
* **Outline** lists the open document's headings, live, parsed from the source as you type,
  indented by level, with the current section highlighted. This is how writers navigate
  prose. It was entirely absent before.
* **Sticky heading**: the heading you are inside stays pinned at the top of the text while
  you scroll (borrowed from VS Code's sticky scroll). In a long section this answers
  "where am I?" without moving your eyes to the sidebar.
* **Breadcrumb** in the header mirrors it: `Projects › project › file › §section`, and each
  segment is clickable.

*Rejected:* numbering the outline `01–05`. Ordinals of a file list mean nothing to an author.

### 4.3 Write

*Goal: type, with nothing in the way.*

* The text column has a **measure limit** (~74ch) and centres in the pane. Full-width
  monospace prose at 1680 px is unreadable; a measure is the single biggest legibility win.
* The source stays monospace and honest — it is Typst, not a rich-text illusion — while
  headings, emphasis, citations, figures and tables get in-place styling (this already
  existed and is kept: it is the right compromise).
* The document bar carries only document-scope facts: name, path, version, and the
  track-changes switch. Nothing project-scope, nothing pane-scope.
* **Focus mode** (`⌘⇧F` or from the palette) collapses navigator, dock, and preview to
  leave only the text. Office suites bury this; writers use it constantly.
* Autosave is unchanged in behaviour, and the header states it plainly: *Saved 12:04* /
  *Saving…* / *Unsaved changes*.

*Rejected:* a formatting toolbar. It would imply WYSIWYG the source does not provide, and
Typst's syntax is the actual model.

### 4.4 Track changes and review

*Goal (author): see what people suggested and accept or reject it.*
*Goal (reviewer): work through a queue without touching the mouse.*

* **One switch** controls track changes, in the document bar, stating its own state:
  `Track changes: on/off`. Reviewers are locked on (server-enforced too) and the switch
  says so on hover. Previously this control existed three times.
* Suggestions render **as text** — green underline for an insertion, red strike-through for
  a deletion, amber ground for a commented span. Never as chips: review state must read as
  the document it will become. (Kept from the old client, and the right call.)
* Clicking a mark opens a **small thread popover** anchored to it — the office-native
  gesture, and the fastest path for one item.
* The **Review dock** is the queue for many items: filter chips (All / Suggestions /
  Comments / Mine), one row per item with author, age, `file:line`, and the excerpt, plus
  Accept/Reject or Resolve. Rows, not cards: a card list drowns at 40 open items.
* **Keyboard triage** inside the dock: `↑`/`↓` move, `Enter` jumps to the text, `A` accepts,
  `R` rejects, `C` comments, `Esc` returns to the text. Reviewers repeat this hundreds of
  times; it should cost one keystroke, not one aimed click. Shortcuts are printed in the
  dock footer, and only bind while focus is inside the dock, so they can never fire mid-word.
* The **count** appears on the Review button and nowhere else when the dock is closed.
* Selecting text offers a **Comment** affordance at the selection — the Google Docs gesture.

*Rejected:* the amber banner. It was a third statement of a fact already stated twice, and
it carried a duplicate track-changes toggle that could disagree with the other two.
*Rejected:* `⌘1`/`⌘2` for accept/reject (from mock B) — those are browser tab switches.

### 4.5 See the rendered page

*Goal: check what it looks like; find the source behind a paragraph.*

* The preview bar carries what the page **is**: which view, which page, and where it came
  from (`build #41 · from version c128`). Provenance is a first-class fact in Nisaba, and
  the writer's version of that fact is "this page is from your 12:04 version".
* The **view switch** lives here, not in a toolbar, because it changes only the artefact:
  `Final · Original · All markup · Public copy` (see the vocabulary table). Switching
  recompiles immediately, which is what asking for a view means.
* Double-click a word in the page to jump to it in the source (kept).
* Zoom controls and page position sit at the right of the bar.

*Rejected:* putting the primary action here, Overleaf-style. The preview pane can be
hidden; the primary action must never be hideable. It lives in the status bar with the
build state it produces.

### 4.6 Update the preview, and deal with problems

*Goal: fix the error that stopped the page from rendering.*

* The primary action is **Update preview** in the status bar, beside the build health it
  affects. `⌘⏎` and `⌘S` both trigger it (`⌘S` saves first — a writer pressing `⌘S` expects
  both).
* Errors and warnings live in the **build drawer**: a bottom strip with `Problems N` and
  `Log` tabs. It opens itself when a build fails and can be pinned open. Each problem shows
  severity, message, and `line 12`, and clicking it selects that span in the text.
* The `Log` tab is the build history — time, status, duration, page count, build id, and
  which engine served (`server` or `in-browser`; the in-browser wasm compile is an
  experimental opt-in, and a tab that opted in but cannot use it logs one line saying why
  it built on the server instead). This is the expert surface, and being a *log* it is
  chronological, which is what a log is for.
* When the drawer is closed, the status bar still states the truth: `✓ 12 pages · 1 warning`
  or `2 problems`, clickable to open the drawer.

*Rejected:* diagnostics squeezed above the PDF (the old design). It stole preview height on
every failure and put source-line errors as far from the source as the layout allows.

### 4.7 Cite something

*Goal: insert a citation without losing my place.*

* Typing `@` or `#cite(<` in the text opens the **inline reference completer** with fuzzy
  matching over authors, year, and title (kept — it is the fastest possible path and needs
  no panel at all).
* The **References dock** is the library view: filter, add, attach a full text, see which
  entries an export would block. At dock width it can show authors, year, and full-text
  state per entry; the old 385 px modal could not.

### 4.8 Look at history

*Goal: see what changed, and get an earlier version back.*

* **History dock**: the version timeline down the panel, the diff below it, at full panel
  height. Selecting one version shows it; selecting a second diffs them.
* Versions read as `12:04 · you` with the checkpoint id as secondary metadata.

*Rejected:* history in a modal. A standing workflow that hides the document behind it
cannot be used *while* writing, which is exactly when it is needed.

### 4.9 Share and roles

*Goal: get the right people in, at the right level.*

* **Presence** in the header: avatar per person, colour derived from the name, hovering
  says who they are and where they are working. The sync protocol has carried a live roster
  with heartbeats all along; the client simply never read it. It does now.
* **Share dock**: invite by username with a role, current members with role badges, and
  shareable links with copy/revoke.
* Role vocabulary stays plain: *Owner · Author · Reviewer · Read-only*, each with a
  one-line description of what it permits in the invite control itself.
* Every role gate that hides a control is unchanged in behaviour — a reviewer never sees
  buttons that would 403.

### 4.10 Export

* **Export dock**: choose the entry document, see which view will be used, run it, and
  download the bundle or the PDF. Results list the files included.

### 4.11 Do the occasional thing (the palette)

`⌘K` opens a fuzzy palette over: files, headings of the open document, references, and
commands (update preview, track changes, focus mode, each dock, each view, sign out…).
Every action in the interface is reachable there, which is what lets the permanent chrome
stay small. Results are grouped and labelled; there is no syntax to learn — but `>` filters
to commands only, for people who arrive from VS Code.

### 4.12 Know that everything is fine

The status bar answers "is anything wrong?" in one glance, left to right:
`● Live · 3 here` — `Ln 12, Col 47` — `4,812 words` — `✓ 12 pages · 1 warning` — `Update preview`.

Sync honesty is preserved: the browser's offline event dims the indicator immediately
rather than waiting for a WebSocket timeout, and the label never claims "Live" when it is
not.

---

## 5. Visual system

The chosen direction is a synthesis: **paper for content, calm neutral for chrome, one
reserved accent, semantic colour only for meaning.**

### Surfaces

| Token | Value | Used for |
|---|---|---|
| `--paper` | `#fdfcf9` | The text column and the rendered sheet — the two surfaces that *are* the document |
| `--chrome` | `#f2f3f0` | Navigator, docks, bars — everything that is apparatus |
| `--shell` | `#e9eae6` | The ground behind the page preview |
| `--ink` | `#1b1e1c` | Body text |
| `--muted` | `#6a706a` | Metadata, micro-labels |
| `--rule` / `--rule-strong` | `#dfe0da` / `#c7c9c1` | Hairlines; the strong one separates regions |

Squared edges (2 px maximum), hairline rules, no gradients, no drop shadows except the one
that makes the rendered page sit on its ground and the one under a floating popover.

### Colour with a job

* **Accent `#23508a`** — primary action, selection, focus ring, active navigation. Nothing else.
* **Green `#2e7d4f`** — insertions. **Amber `#a4761b`** — comments and warnings.
  **Red `#a63a2e`** — deletions and errors.
* Semantic colours are never used decoratively, and the accent is never used semantically.
  This is why the accent is blue rather than mock A's red pencil or mock B's press green:
  both collide with a review meaning.
* Author identity uses a deterministic hue per name (kept from the old client) — the one
  place colour is generated rather than chosen.

### Type

* UI: system sans (`DM Sans` when installed). Metadata and micro-labels: mono
  (`IBM Plex Mono`/`ui-monospace`), uppercase, `0.14em` tracking, 9–10 px.
* Source: mono, 14 px, 1.75 line height, measure-limited.
* Rendered page: serif — it is a document, not an interface.
* No web fonts are fetched: the app must work offline and must not phone home.
* Numbers that sit in columns use `font-variant-numeric: tabular-nums`.

### Accessibility

Everything the old client got right is kept and extended: skip link, ARIA roles and live
regions, visible focus rings on the accent, `prefers-reduced-motion`, `prefers-contrast`,
forced-colours support, a print stylesheet that prints the artefact and not the chrome.
New: the review queue is a proper listbox with roving `tabindex`, the palette is a combobox
with `aria-activedescendant`, and every icon-only control has a label. Colour is never the
only carrier of meaning — insertions are underlined, deletions struck, comments marked.

---

## 6. Keyboard model

| Key | Action | Scope |
|---|---|---|
| `⌘K` / `Ctrl K` | Command palette | Global |
| `⌘⏎` | Update preview | Global |
| `⌘S` | Save, then update preview | Global |
| `⌘⇧F` | Focus mode | Global |
| `⌘B` | Navigator | Global |
| `⌘=` / `⌘−` | Zoom the preview | Pointer over the preview pane |
| `↑` `↓` `Enter` `A` `R` `C` `Esc` | Triage the review queue | Review dock only |
| `Esc` | Close the palette, popover, or dock | Contextual |

Single-letter shortcuts bind **only** while focus is inside the review dock, so they can
never fire into the text.

**Browser defaults are not hijacked.** No chord the browser needs is intercepted:
the reload family (`⌘R`, `⌘⇧R`, `F5`), devtools, and history all reach the browser
untouched — the Review dock deliberately has no global chord (button and palette
command instead), and the zoom chords act on the preview only while the pointer is
over the preview pane, leaving page zoom working everywhere else. The editor-standard
overrides the app does keep (`⌘S`, `⌘K`, `⌘B`, `⌘⇧F`) replace browser actions that are
meaningless in the app.

---

## 7. Deliberately not done

* **No dark theme.** It is a real request for long sessions, but it is a separate,
  system-wide piece of work (every token, the rendered page, the PDF ground) and doing it
  badly is worse than not doing it. The stylesheet is fully tokenised so it is a contained
  follow-up.
* **No mobile layout.** The workspace collapses to a readable single column below 900 px,
  but authoring on a phone is not a goal.
* **No minimap, no split editors, no multi-file tabs.** Borrowed from code editors only
  where prose benefits: palette, sticky heading, focus mode, fuzzy open.
* **No pipeline strip** (mock C). Attractive for the domain, but it restates the build and
  review facts the status bar already owns, and drafting is calmer without it.
