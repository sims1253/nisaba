# Nisaba user guide

A plain-language guide to using Nisaba as an author, reviewer, or reader. It
covers the web app at the URL your administrator gives you (local dev:
http://127.0.0.1:8103). Operator concerns (backups, health, deployment) live in
[`operations.md`](operations.md); the domain vocabulary is in
[`CONTEXT.md`](../CONTEXT.md).

## Signing in

Click **Sign in** and complete the login at your identity provider (local dev
uses Keycloak with the demo accounts `demo`/`demo`, `reviewer`/`reviewer`,
`reader`/`reader`). You are signed in when the toolbar shows **Sign out**.

> Sessions: access tokens are short-lived (5 minutes in the local dev realm).
> The app refreshes them in the background; if an API call fails with a 401,
> reload the page. You may be prompted to sign in again.

## Projects and documents

- **Projects** are workspaces. Use **＋** (top of the sidebar) to create one
  (owners and authors only — reviewers and read-only members view projects
  they are invited or linked into).
- Inside a project, the sidebar lists **documents** at their paths
  (`chapters/01-intro.typ`). Folders are derived from the paths — create a
  document with a nested path to organise it.
- **Add document** creates a new file; click a document to open it in the
  editor.
- **Compile ⌘⏎ (Ctrl+Enter)** builds the PDF. Errors and warnings appear in the
  diagnostics panel with file, line, and message; clicking one jumps to the
  location. A green check means the build succeeded and the preview shows the
  rendered pages.

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

1. Select text and type, or select and replace: your edit becomes a
   **suggestion** instead of changing the document.
2. The **Review** button shows the number of open items; open it for the item
   list. Every item has **Accept** / **Reject** (plus **Accept all** /
   **Reject all** at the bottom).
3. **Add comment** inside the Review panel leaves an anchored comment at your
   cursor or selection. Comments appear in the same list for everyone, and
   anyone can add further comments (or **Resolve** a thread once it is done).
4. Accepting/rejecting applies or discards the suggestion; the baseline text
   changes only then. Suggestions are synced to collaborators in real time and
   survive reloads (they are stored in the shared document state, not in the
   plain text).

> Suggestions never modify the underlying text directly — that is what makes
> accept/reject possible. If an item shows **anchor lost** or **Text already
> removed · Reject restores it**, the surrounding text changed underneath it;
> accepting/rejecting still works and the label tells you what will happen.

## Sharing

- **Share** (owner/author only) opens the invite panel: type a username, pick a
  role, click **Invite**. Members appear in the list with a **Remove** button
  (the owner row cannot be removed).
- **Create link** makes a shareable URL that grants access at a chosen role to
  anyone who opens it while signed in. **Revoke** invalidates a link
  immediately; revoked links no longer grant access.

## References

- **References** (owner/author) lets you add structured citation metadata
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
| Compile failed — See diagnostics for details | The Typst source has errors; the diagnostics panel shows file/line/message. Headings use `=` (`= Introduction`), not `#`. |
| Page doesn't react after signing in | Reload the page; the token refresh may have been interrupted. |

## Keyboard shortcuts

- `⌘⏎` / `Ctrl+Enter` — Compile
- Standard CodeMirror editing keys: undo/redo, find, multi-cursor
- Editor autocompletion is available as you type Typst constructs.
