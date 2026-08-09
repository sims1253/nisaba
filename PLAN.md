# Nisaba — Product Direction

Nisaba is a self-hostable, collaborative authoring environment for general document projects.
It combines a source editor, structured review, real-time collaboration, reproducible builds,
and durable history without assuming a particular industry, template, or document shape.

## Principles

1. **Projects contain files.** A project is an ordered tree of text and binary files addressed
   by stable project-relative paths. Folders are organizational, not domain concepts.
2. **Text has one authority.** Collaborative CRDT state is canonical; database text is a derived
   read model for indexing and search.
3. **Review is structured.** Suggestions, comments, decisions, and anchors are typed entities,
   not formatting conventions or one serialized blob.
4. **Builds name immutable input.** Preview, history, restore, comparison, and export refer to
   checkpoints containing exact file frontiers, assets, settings, and toolchain versions.
5. **Compilers are adapters.** Typst is the first engine. The project and review models do not
   encode assumptions from a compiler or a specific kind of document.
6. **Large projects stay incremental.** Files, previews, and artifacts stream independently;
   large binary payloads do not travel as base64 JSON.
7. **Self-hosting is ordinary.** One-machine deployment is the baseline, with documented backup,
   restore, upgrade, and an optional path to high availability.

## Core model

```text
Organization
└── Project
    ├── File tree
    │   ├── Text document → collaborative lineage
    │   └── Binary asset  → content-addressed blob
    ├── Entrypoints
    ├── References
    ├── Checkpoints
    └── Build artifacts
```

A text document contains:

- collaborative text,
- review marks and discussion threads,
- structured data used by templates or extensions,
- stable history through checkpoints.

No built-in hierarchy exists above a document beyond the general file tree. Applications may
provide optional templates or extensions, but those cannot change the platform's core model.

## Authoring loop

1. Open any text file in CodeMirror.
2. Apply local edits immediately to the Loro replica.
3. Persist accepted updates durably before acknowledging them.
4. Resolve comments and suggestions through deterministic shared semantics.
5. Build a selected entrypoint from an immutable checkpoint.
6. Stream diagnostics and preview frames back to the client.
7. Store final artifacts by content hash with provenance linking them to the checkpoint.

## Quality bar

- Unicode-safe position conversion across Rust, JavaScript, CRDT anchors, and compiler spans
- convergence across offline edits, reconnects, crashes, and compaction
- explicit authorization for every project, file, build, and collaboration operation
- deterministic build inputs and named exceptions where output cannot be byte-identical
- accessible keyboard and screen-reader workflows
- bounded CPU, memory, time, output size, and file count for compiler workers
- verified backup, restore, upgrade, and rollback procedures

Current implementation status and remaining work live in [`ROADMAP.md`](ROADMAP.md).
