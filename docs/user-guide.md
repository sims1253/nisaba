# Nisaba user guide

## Write your first document

1. Open the URL your administrator gives you and click **Sign in**.
   For a local installation, follow [setup](operations.md#1-quick-start), then
   open http://127.0.0.1:8103 and use `demo` / `demo`.
2. Choose **New project** and give it a name. Nisaba creates `main.typ`.
   If you are a reviewer or read-only user, open a project shared with you.
3. Open the file and write some Typst:

   ```typst
   = Introduction

   Our first shared document.
   ```

4. Choose **Build from** in the sidebar, then click **Update preview**. Opening another file keeps the same compilation entrypoint.
5. Check the save status in the top bar before leaving.

Keep a disconnected tab open and copy unsaved text somewhere safe. Unsaved edits
can be lost on reload or close ([#67](https://github.com/sims1253/nisaba/issues/67)).

## Find your way around

```text
Top bar: project / file / section · tools · save status
┌───────────┬─────────────┬──────────────┬─────────────┐
│ Files and │ Editor      │ Tool panel   │ PDF preview │
│ headings  │             │ (when open)  │             │
└───────────┴─────────────┴──────────────┴─────────────┘
Problems and build log (when open)
Status bar: connection · cursor · word count · Update preview
```

Click a heading or breadcrumb to navigate. Hover a collaborator's avatar to
see their file and section. Close a tool panel with **×**, or press `Esc`
while it has focus.

In **Files**, click **＋** to add a file or double-click its name to rename it.
Paths such as `chapters/intro.typ` appear as folders.

Press `⌘K` / `Ctrl+K` to find files, headings, references, or actions.
Use `⌘F` / `Ctrl+F` to search text in the open file. Project-wide text search
is not available yet ([#63](https://github.com/sims1253/nisaba/issues/63)).

## Review changes

1. Turn on **Track changes** above the text. Reviewers always have it on.
   Insertions are underlined; deletions are struck through.
2. Open **Review** to **Accept** or **Reject** suggestions, individually or all
   at once. Accepting keeps a change; rejecting undoes it.
3. Select text and choose **Comment**, or use **Add a comment here** in the
   panel. Reply to a thread or **Resolve** it when the discussion is complete.

Suggestions and comments sync to collaborators. Wait for sync before closing.
**needs re-anchoring** means the surrounding text changed. A deletion labeled
**Already removed from the text — Reject puts it back** restores its text when rejected.

With the review queue focused, use `↑`/`↓` to move, `Enter` to show an item,
`A` to accept, `R` to reject, `C` to comment, and `Esc` to return to writing.

## Choose a preview version

The preview switch selects the view for the next build. **Edited since preview** means the document has changed since that PDF was built. **Download PDF** above the preview downloads the displayed PDF without building it again. If an update fails, the last successful PDF stays visible while you fix the problems.

| Version | What you see |
|---------|--------------|
| Final | All suggestions applied |
| Original | All suggestions undone |
| All markup | Insertions and deletions marked |
| Public copy | Final, with redacted passages removed |

## Share a project

Open **Share**, enter a username, choose a role, and click **Invite**.
**Create link** lets signed-in users join with the chosen role. Copy the link
when it appears; its secret is shown once.

**Revoke** stops future use of a link. To withdraw an existing member's access,
use **Remove**. The owner cannot be removed.

Your sign-in role and project role must both allow the action:

| Action | Owner / author | Reviewer | Read-only |
|--------|:--------------:|:--------:|:---------:|
| Read files, history, audit, and members; preview | Yes | Yes | Yes |
| Edit text | Yes | Suggestions only | No |
| Accept, reject, and comment | Yes | Yes | No |
| Add, rename, or delete files; manage references | Yes | No | No |
| Export | Yes | Yes | No |
| Manage access or delete the project | Yes | No | No |

Local test accounts are `demo` / `demo`, `reviewer` / `reviewer`, and
`reader` / `reader`. See the [role model](architecture.md#61-role-model) for details.

## Cite references and export

In **References**, add the title, authors, year, DOI, and journal. Each DOI
must be unique within the project. Use **Insert citation** to cite an entry
at the cursor and **Attach PDF** to add its full text.

Use **Download PDF** above the preview to download the PDF on screen. It remains
that version even if someone has edited the document since the build.

Open **Export**, choose an entry document, and click **Prepare download** to build
a new PDF and source archive from current collaborative project files. The archive
includes the projected Typst sources and generated bibliography needed by that build.
Cited references do not need full-text attachments for this ordinary export.

API clients can request `include_fulltexts: true` on an export to add the reference
evidence bundle. This option requires a PDF for every cited reference.

## History and settings

**History** lists saved versions of the open file. Pick one to read it; pick a
second to compare them.

**Settings** changes editor typeface, size, spacing, and app shortcuts in this
browser. These preferences do not change the PDF. **Opening file** sets a
local preference for a project; a more recent file in the current tab takes precedence.

## When something goes wrong

| Symptom | Next step |
|---------|-----------|
| Disconnected, save conflict, or sign-in failure | Keep the tab open and copy unsaved text before reloading or signing in again. Compare it with the saved version before reapplying edits. |
| Permission denied or sync error 4003 | Ask the owner to check membership and your administrator to check sign-in access. Token expiry can also cause 4003 ([#61](https://github.com/sims1253/nisaba/issues/61)). |
| Preview or export fails to compile | Read the Problems panel. Click a problem to open the named project file. Generated sources may need to be checked in the downloaded archive. |

For a bug report, include the error message and the steps that led to it.
The [issue tracker](https://github.com/sims1253/nisaba/issues) lists known problems.

## Keyboard shortcuts

These are the defaults; app shortcuts can be changed in **Settings**.

| Key | Action |
|-----|--------|
| `⌘K` / `Ctrl+K` | Find files, headings, references, and actions |
| `⌘F` / `Ctrl+F` | Find text in the open file |
| `⌘D` / `Ctrl+D` | Add the next occurrence to the selection |
| `⌘⏎` / `Ctrl+Enter` | Update preview |
| `⌘S` / `Ctrl+S` | Save, then update preview |
| `⌘⇧F` / `Ctrl+Shift+F` | Toggle focus mode |
| `⌘\` / `Ctrl+\` | Show or hide the sidebar |
| `⌘=` / `⌘−` | Zoom while the pointer is over the preview |

Undo/redo and multi-cursor editing use CodeMirror's standard shortcuts.
Autocomplete suggests Typst constructs and references. Browser reload, developer
tools, and zoom outside the preview keep their usual shortcuts.
