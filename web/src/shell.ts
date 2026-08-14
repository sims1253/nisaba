/**
 * The workspace shell: static markup for every region of the UI.
 *
 * It lives in its own module so `main.ts` stays behaviour, not layout, and so the
 * structure documented in docs/ui-design.md is readable in one place. Everything
 * here is static — no interpolation of any kind, so there is nothing to escape.
 * Dynamic regions are empty containers that the render functions in `main.ts`
 * fill.
 *
 * Region order matches the reading order of the design doc:
 *   app bar · projects screen · workspace (navigator | document | dock | preview)
 *   · build drawer · status bar · modal.
 */

export const SHELL_HTML = `
<header class="appbar">
  <button class="brand" id="go-projects" type="button" title="All projects">
    <span class="mark" aria-hidden="true">N</span><span>Nisaba</span>
  </button>
  <nav class="crumbs" id="crumbs" aria-label="Location"></nav>

  <button class="palette-hint" id="open-palette" type="button" aria-label="Search files, sections and commands">
    <span class="lead" aria-hidden="true">⌕</span>
    <span class="text">Search files, sections, commands…</span>
    <kbd>⌘K</kbd>
  </button>

  <div class="appbar-tools" id="appbar-tools">
    <button id="references-button" class="btn btn-quiet" type="button" title="The project's reference library">References</button>
    <button id="history-button" class="btn btn-quiet" type="button" title="Earlier versions of this file" hidden>History</button>
    <button id="share-button" class="btn btn-quiet" type="button" title="People with access" hidden>Share</button>
    <button id="export-button" class="btn btn-quiet" type="button" title="Download the finished document">Export</button>
  </div>

  <div class="appbar-right">
    <div class="presence" id="presence" aria-live="polite" aria-label="People in this document"></div>
    <span class="save-status" id="save-status">Ready</span>
    <button id="sign-in" class="btn btn-quiet" type="button">Sign in</button>
  </div>
</header>

<main class="screen" id="projects-screen">
  <div class="screen-inner">
    <div class="screen-head">
      <h2>Projects</h2>
      <button id="new-project" class="btn btn-primary" type="button">New project</button>
    </div>
    <p class="screen-lede">A project holds the files, references, and history of one document.</p>
    <div id="project-list"></div>
  </div>
</main>

<div class="workspace" id="workspace">
  <aside class="navigator" id="navigator" aria-label="Files and sections">
    <div class="nav-section nav-section-files">
      <h2 class="nav-head">
        <span>Files</span><span class="count num" id="file-count"></span>
        <span class="nav-head-actions">
          <button id="add-document" class="btn-icon" type="button" title="New file" aria-label="New file">＋</button>
          <button id="add-demo" class="btn-icon" type="button" title="Add the demo document" aria-label="Add the demo document">✧</button>
          <button id="hide-navigator" class="btn-icon" type="button" title="Hide the sidebar (⌘B)" aria-label="Hide the sidebar">‹</button>
        </span>
      </h2>
      <div class="nav-body" id="file-tree"></div>
    </div>
    <div class="nav-section nav-section-outline">
      <h2 class="nav-head"><span>Outline</span></h2>
      <div class="nav-body" id="section-outline"></div>
    </div>
    <div class="nav-foot" id="nav-foot"></div>
  </aside>

  <div class="gutter" data-gutter="navigator" title="Drag to resize · double-click to hide"></div>

  <section class="doc-pane" aria-label="Document">
    <div class="pane-bar doc-bar doc-chrome">
      <div class="doc-id">
        <strong id="document-name">No document open</strong>
        <code id="document-path"></code>
      </div>
      <div class="doc-bar-right">
        <span class="doc-rev num" id="revision-label"></span>
        <button id="suggesting-button" class="switch" type="button" role="switch" aria-checked="false"
                title="Record your edits as suggestions instead of changing the text">Track changes: off</button>
        <button id="review-button" class="btn" type="button" aria-expanded="false" title="Comments and suggested changes (⌘⇧R)">
          Review<span class="count-badge num" id="review-count" hidden></span>
        </button>
      </div>
    </div>
    <div class="sticky-heading doc-chrome" id="sticky-heading" hidden></div>
    <div id="editor" class="editor-host doc-chrome"></div>
    <div class="pane-empty" id="editor-placeholder">
      <h2>No document open</h2>
      <p>Pick a file on the left, or press ⌘K to search for one.</p>
    </div>
  </section>

  <div class="gutter" data-gutter="dock" title="Drag to resize" hidden></div>

  <aside class="dock" id="dock" aria-label="Tools" hidden>
    <div class="pane-bar">
      <h2 id="dock-title">Review</h2>
      <button id="dock-close" class="btn-icon" type="button" style="margin-left:auto" title="Close (Esc)" aria-label="Close panel">×</button>
    </div>
    <div class="dock-body" id="dock-content"></div>
    <div class="dock-foot" id="dock-foot" hidden></div>
  </aside>

  <div class="gutter" data-gutter="preview" title="Drag to resize · double-click to hide"></div>

  <section class="preview-pane" aria-label="Page preview">
    <div class="pane-bar preview-chrome">
      <div class="view-switch" id="view-switch" role="group" aria-label="Which version to render"></div>
      <span class="build-label" id="build-label">No preview yet</span>
      <span class="page-position num" id="page-position"></span>
      <div class="zoom-controls" id="pdf-zoom-controls" hidden>
        <button id="zoom-out" class="zoom-button" type="button" title="Zoom out (⌘−)" aria-label="Zoom out">−</button>
        <span class="zoom-level num" id="zoom-level">125%</span>
        <button id="zoom-in" class="zoom-button" type="button" title="Zoom in (⌘+)" aria-label="Zoom in">+</button>
        <button id="zoom-reset" class="zoom-button" type="button" title="Fit the page" aria-label="Fit the page">⟲</button>
      </div>
      <button id="hide-preview" class="btn-icon" type="button" title="Hide the preview" aria-label="Hide the preview">›</button>
    </div>
    <div id="pdf-viewer" class="pdf-viewer preview-chrome"></div>
    <div class="pane-empty" id="preview-placeholder">
      <h2>No preview yet</h2>
      <p>Open a document and choose Update preview to see the rendered pages.</p>
    </div>
  </section>
</div>

<section class="drawer" id="build-drawer" aria-label="Problems and build log" hidden>
  <div class="drawer-tabs" role="tablist">
    <button id="drawer-tab-problems" class="tab" type="button" role="tab" aria-selected="true" data-drawer-tab="problems">
      Problems<span class="n num" id="problem-count">0</span>
    </button>
    <button id="drawer-tab-log" class="tab" type="button" role="tab" aria-selected="false" data-drawer-tab="log">Log</button>
    <button id="drawer-close" class="btn-icon" type="button" title="Close" aria-label="Close the problems panel">×</button>
  </div>
  <div class="drawer-body" id="diagnostics-list" role="tabpanel" aria-labelledby="drawer-tab-problems"></div>
  <div class="drawer-body" id="build-log" role="tabpanel" aria-labelledby="drawer-tab-log" hidden></div>
</section>

<footer class="statusbar">
  <button class="status-cell" id="connection-state" type="button" title="Collaboration status">
    <span class="status-dot" id="status-dot" data-state="disconnected"></span>
    <span id="sync-label">No document</span>
  </button>
  <span class="status-cell num" id="cursor-position">Ln 1, Col 1</span>
  <span class="status-cell num" id="word-count">0 words</span>
  <span class="status-cell status-spacer"></span>
  <div class="right">
    <button class="status-cell" id="build-health" type="button" title="Problems and build log">No preview yet</button>
    <button id="compile-button" class="btn btn-primary" type="button" title="Compile the document and refresh the preview">
      Update preview <kbd>⌘↵</kbd>
    </button>
  </div>
</footer>

<button id="show-navigator-tab" class="show-pane-tab show-pane-tab-left" type="button" title="Show the sidebar" aria-label="Show the sidebar" hidden>›</button>
<button id="show-preview-tab" class="show-pane-tab show-pane-tab-right" type="button" title="Show the preview" aria-label="Show the preview" hidden>‹</button>

<dialog id="workspace-panel" class="modal">
  <form method="dialog">
    <div class="modal-head">
      <div>
        <span class="eyebrow" id="panel-eyebrow">Workspace</span>
        <h2 id="panel-title">Panel</h2>
      </div>
      <button class="close-button" value="cancel" aria-label="Close">×</button>
    </div>
    <div id="panel-content"></div>
  </form>
</dialog>
`
