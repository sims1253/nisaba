# Nisaba user guide

A plain-language guide to using Nisaba as an author, reviewer, or reader. It
covers the web app at the URL your administrator gives you (local dev:
http://127.0.0.1:8103). Operator concerns (backups, health, deployment) live in
[`operations.md`](operations.md); the domain vocabulary is in
[`CONTEXT.md`](../CONTEXT.md).

## Signing in

Click **Sign in** and complete the login at your identity provider (local dev
uses Keycloak with the demo accounts `demo`/`demo`, `reviewer`/`reviewer`,
`reader`/`reader`). You are signed in when the top bar shows **Sign out**.

> Sessions: access tokens are short-lived (5 minutes in the local dev realm).
> The app refreshes them in the background; if an API call fails with a 401,
> reload the page. You may be prompted to sign in again.

## The workspace

Opening a project gives you five regions:

- the **top bar** — where you are (the breadcrumb `project › file › §section`,
  each part clickable), who else is here (avatars; hover one to see the file and
  section they are in), whether your work is saved, and the project actions
  (References, History, Share, Export);
- the **sidebar** — **Files** as a folder tree derived from the document paths,
  and below it the **Outline** of the open file's headings. Click a heading to
  jump to it; the section you are in stays pinned above the text as you scroll;
- the **text** in the middle;
- the **panel** on the right — one tool at a time (Review, References, History,
  Share, Export); close it with its × or `Esc` to give the text more room;
- the **preview** — the rendered pages, with a switch for which version to show;
- the **status bar** along the bottom — connection, cursor, word count, whether
  the preview is up to date, and **Update preview**.

## Projects and documents

- **Projects** hold the files, references, and history of one document. Create
  one with **New project** on the projects screen (owners and authors only —
  reviewers and read-only members see the projects they are invited or linked
  into).
- Inside a project, **Files** lists the documents at their paths
  (`chapters/01-intro.typ`). Folders come from the paths, so creating a document
  with a nested path organises it. The file the preview builds from carries a
  **MAIN** tag.
- **＋** in the Files header adds a file; click a file to open it; double-click
  it to rename it.
- **Update preview** (`⌘⏎` / `Ctrl+Enter`) builds the pages. Problems appear in
  the panel at the bottom with file, line, and message; clicking one jumps to
  the place in the text. The status bar says whether the preview is up to date.
- **Press `⌘K` / `Ctrl+K`** to search files, sections, and references, or to run
  any action by name — including everything described below.

## Roles

What you can do depends on your **project role** (set by the owner in the Share
panel):

| Action | Owner | Author | Reviewer | Read-only |
|--------|:-----:|:------:|:--------:|:---------:|
| Read and compile | ✓ | ✓ | ✓ | ✓ |
| Edit the text directly | ✓ | ✓ | — (suggest only) | — |
| Accept / reject / comment on suggestions | ✓ | ✓ | ✓ | — |
| Create / rename / delete documents | ✓ | ✓ | — | — |
| Invite / remove members, share links, export, delete project | ✓ | ✓ | — | — |

If an action is not available to your role it is hidden; if you call it through
the API you get a 403 with the message "You don't have permission to do that".

## Reviewing (track changes)

Reviewers are always in **suggesting mode** (`Track changes: on`).

1. Turn on **Track changes** in the bar above the text (reviewers are always on
   and cannot turn it off). Then type: your edit becomes a **suggestion**
   instead of changing the document.
2. The **Review** button shows the number of open items; open it for the queue.
   Every item has **Accept** / **Reject** (plus **Accept all** / **Reject all**
   at the bottom). Suggestions also show in the text itself — an insertion is
   underlined, a deletion struck through, a commented passage marked.
3. With the queue focused you can work through it without the mouse: `↑`/`↓`
   move, `Enter` shows the item in the text, `A` accepts, `R` rejects, `C`
   comments, `Esc` returns to writing.
4. Select text and choose **Comment**, or use **Add a comment here** in the
   panel, to leave an anchored comment. Comments appear in the same queue for
   everyone, and
   anyone can add further comments (or **Resolve** a thread once it is done).
5. Accepting/rejecting applies or discards the suggestion; the baseline text
   changes only then. Suggestions are synced to collaborators in real time and
   survive reloads (they are stored in the shared document state, not in the
   plain text).

> Suggestions never modify the underlying text directly — that is what makes
> accept/reject possible. If an item shows **needs re-anchoring** or **Already
> removed from the text — Reject puts it back**, the surrounding text changed
> underneath it; accepting/rejecting still works and the label tells you what
> will happen.

## Which version the preview shows

The switch on the preview bar chooses what gets rendered — and what an export
contains:

| Switch | What you see |
|--------|--------------|
| **Final** | Every suggested change applied — what the document becomes |
| **Original** | No suggested changes applied — the last agreed text |
| **All markup** | The text with insertions and deletions marked |
| **Public copy** | Final, with redacted passages removed |

Changing the switch rebuilds the preview straight away.

## Sharing

- **Share** (owner/author only) opens the invite panel: type a username, pick a
  role, click **Invite**. Members appear in the list with a **Remove** button
  (the owner row cannot be removed).
- **Create link** makes a shareable URL that grants access at a chosen role to
  anyone who opens it while signed in. **Revoke** invalidates a link
  immediately; revoked links no longer grant access.

## References

- **References** (owner/author) opens the library panel, where you add structured
  citation metadata
  (title, authors, year, DOI, journal). DOIs must be unique per project.
- Export requires every cited reference to have an uploaded full-text PDF; the
  portable archive contains the PDF, the document sources, and per-document
  RIS bibliographies with full-text attachments.

## Troubleshooting

| Symptom | Meaning / what to do |
|---------|----------------------|
| "Sync unavailable · Sync server error 4003: …" | The real-time relay could not connect (usually a key/issuer mismatch on the server). Your local edits are still saved on close; ask your administrator to check the sync service logs. Reload to retry. |
| "You don't have permission…" | Your role does not allow the action. Ask the project owner to change your role in the Share panel. If you are a reviewer, this message should not appear while suggesting — your edits are synced as suggestions, not saved as baseline text. |
| Save conflict — another author edited this document | Another collaborator's saved version conflicts with yours; reload to merge (your unsaved text is preserved in the editor). |
| "N problems stopped the preview" | The Typst source has errors; the Problems panel at the bottom shows file, line, and message, and clicking one jumps there. Headings use `=` (`= Introduction`), not `#`. |
| Page doesn't react after signing in | Reload the page; the token refresh may have been interrupted. |

## History

**History** lists the saved versions of the open file, newest first. Pick one to
read it; pick a second to see what changed between them.

## Keyboard shortcuts

| Key | Action |
|-----|--------|
| `⌘K` / `Ctrl+K` | Search files, sections, references — or run any action |
| `⌘⏎` / `Ctrl+Enter` | Update the preview |
| `⌘S` / `Ctrl+S` | Save, then update the preview |
| `⌘⇧F` / `Ctrl+Shift+F` | Focus mode — hide everything but the text |
| `⌘⇧R` / `Ctrl+Shift+R` | Open the review queue |
| `⌘B` / `Ctrl+B` | Show or hide the sidebar |
| `⌘=` / `⌘−` | Zoom the preview in and out |
| `↑` `↓` `Enter` `A` `R` `C` `Esc` | Work through the review queue (while it has focus) |

Standard editing keys (undo/redo, find, multi-cursor) work as they do anywhere,
and autocompletion offers Typst constructs and your references as you type.
