// Auto-generiertes Typst-Template-Skeleton.
// Quelle: sample-document.docx
// Manifest-Hash (sha256, kanonisch): d082be3ac7ca76900e0eb237cff7b0b861ca3417add569987debe7148a616923
// Generator: nisaba-tools@0.1.0
// HINWEIS: Dies ist ein Skeleton, keine Fidelity-Garantie. Visuelle
// Übereinstimmung wird NUR durch den page-image-diff gegen eine echte
// DOCX-Renderung behauptet (siehe docs/template-pipeline.md).

#let felder = (
  author: "<<Author>>",
  project: "<<Project>>",
  version: "<<Version>>",
)

#set page(
  width: 595.3pt,
  height: 841.9pt,
  margin: (top: 56.7pt, bottom: 56.7pt, left: 56.7pt, right: 56.7pt),
  header: align(right)[Nisaba Skeleton],
  numbering: "1",
)
#set text(lang: "de", font: "Libertinus Serif")
#set par(justify: true)

#set heading(numbering: "1.1.1")
#show heading.where(level: 1): it => pagebreak(weak: true) + it

// Erlaubte Funktionsnamen: figure, table, cite.
#let _allowlist = ("figure", "table", "cite")

=== Dokument ===

= Sample Document

== Table of Contents

(Table of Contents)

Author: <<Author>>

<<Project>>

External Link

#figure(table(columns: 2)[
  // Tabelle 1 aus dem DOCX
][
  <<Tabelle_Inhalt>>
], caption: [Tabelle 1])

First item

Second item

#pagebreak()

=== List of Tables

=== List of Figures

