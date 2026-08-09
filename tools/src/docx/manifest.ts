/**
 * The deterministic intermediate manifest.
 *
 * This is the single contract between DOCX introspection and everything
 * downstream (Typst skeleton generation, schema/placeholder validation, visual
 * diff provenance). It MUST be stable and order-independent so that:
 *
 *   - re-introspecting the same DOCX yields a byte-identical manifest;
 *   - a saved manifest can be loaded back and diffed against a re-derived one —
 *     the refresh check ("re-deriving the template from a fresh
 *     DOCX is a scripted run producing a diff report").
 *
 * Stability rules:
 *   - `parts` and `requiredPlaceholders` are sorted sets.
 *   - All other arrays preserve document order (content order is meaningful).
 *   - No timestamps; the only path emitted is the source `fileName`.
 *
 * Bump {@link MANIFEST_SCHEMA_VERSION} and note the change in
 * `docs/template-pipeline.md` whenever the shape changes.
 */
import { Schema } from "effect";

export const MANIFEST_SCHEMA_VERSION = "1" as const;

const optional = Schema.optional;

const Orientation = Schema.Literals(["portrait", "landscape"]);
const orientation = Orientation;
const PlaceholderKind = Schema.Literals(["bracket", "sdt", "mergefield", "formfield"]);
const FieldType = Schema.Literals([
  "hyperlink",
  "toc",
  "pageref",
  "ref",
  "mergefield",
  "noteref",
  "bookmark",
  "formfield",
  "other",
]);

// ---------------------------------------------------------------------------
// Page geometry (twips = 1/20 pt, OOXML's native unit).
// ---------------------------------------------------------------------------

export const SectionSchema = Schema.Struct({
  index: Schema.Number,
  pageSize: Schema.Struct({
    widthTwips: Schema.Number,
    heightTwips: Schema.Number,
    orientation,
  }),
  margins: Schema.Struct({
    topTwips: Schema.Number,
    bottomTwips: Schema.Number,
    leftTwips: Schema.Number,
    rightTwips: Schema.Number,
    headerTwips: Schema.Number,
    footerTwips: Schema.Number,
  }),
  columns: Schema.Struct({ count: Schema.Number, spaceTwips: Schema.Number }),
  headerRefs: Schema.Array(Schema.String),
  footerRefs: Schema.Array(Schema.String),
});

// ---------------------------------------------------------------------------
// Content blocks (ordered body model). Union discriminated on `kind`.
// ---------------------------------------------------------------------------

const HeadingBlock = Schema.Struct({
  kind: Schema.Literal("heading"),
  level: Schema.Number,
  text: Schema.String,
  styleId: optional(Schema.String),
});

const ParagraphFlags = Schema.Struct({
  hasHyperlink: Schema.Boolean,
  hasField: Schema.Boolean,
  hasImage: Schema.Boolean,
  hasList: Schema.Boolean,
});

const ParagraphBlock = Schema.Struct({
  kind: Schema.Literal("paragraph"),
  styleId: optional(Schema.String),
  text: Schema.String,
  flags: ParagraphFlags,
});

const TableBlock = Schema.Struct({ kind: Schema.Literal("table"), ref: Schema.Number });
const ImageBlock = Schema.Struct({ kind: Schema.Literal("image"), ref: Schema.Number });
const PlaceholderBlock = Schema.Struct({
  kind: Schema.Literal("placeholder"),
  token: Schema.String,
  placeholderKind: PlaceholderKind,
});
const FieldBlock = Schema.Struct({
  kind: Schema.Literal("field"),
  fieldType: FieldType,
  instruction: Schema.String,
});
const PageBreakBlock = Schema.Struct({ kind: Schema.Literal("pageBreak") });
const SectionBreakBlock = Schema.Struct({ kind: Schema.Literal("sectionBreak"), sectionIndex: Schema.Number });

export const BlockSchema = Schema.Union([
  HeadingBlock,
  ParagraphBlock,
  TableBlock,
  ImageBlock,
  PlaceholderBlock,
  FieldBlock,
  PageBreakBlock,
  SectionBreakBlock,
]);

// ---------------------------------------------------------------------------
// Summaries.
// ---------------------------------------------------------------------------

export const PlaceholderSchema = Schema.Struct({
  token: Schema.String,
  kind: PlaceholderKind,
  count: Schema.Number,
  sample: optional(Schema.String),
});

export const FieldSchema = Schema.Struct({
  type: FieldType,
  instruction: Schema.String,
  count: Schema.Number,
});

export const TableSummarySchema = Schema.Struct({
  index: Schema.Number,
  rows: Schema.Number,
  columns: Schema.Number, // max columns across rows
  styleId: optional(Schema.String),
  hasHeaderRow: Schema.Boolean,
});

export const ImageSummarySchema = Schema.Struct({
  index: Schema.Number,
  relId: optional(Schema.String),
  part: optional(Schema.String), // media part path, if resolvable
  widthEmu: optional(Schema.Number),
  heightEmu: optional(Schema.Number),
});

export const NumberingDefSchema = Schema.Struct({
  numId: Schema.Number,
  abstractNumId: Schema.Number,
  styleLink: optional(Schema.String),
  levels: Schema.Number,
});

export const PartRefSchema = Schema.Struct({
  part: Schema.String,
  type: Schema.Literals(["default", "first", "even"]),
  sha256: Schema.String,
});

export const NoteSummarySchema = Schema.Struct({ count: Schema.Number });

export const StyleSummarySchema = Schema.Struct({
  id: Schema.String,
  name: optional(Schema.String),
  basedOn: optional(Schema.String),
  next: optional(Schema.String),
  isDefault: Schema.Boolean,
  uiPriority: optional(Schema.Number),
});

// ---------------------------------------------------------------------------
// Top-level manifest.
// ---------------------------------------------------------------------------

export const ManifestSchema = Schema.Struct({
  schemaVersion: Schema.Literal(MANIFEST_SCHEMA_VERSION),
  generatedBy: Schema.String,

  source: Schema.Struct({
    fileName: Schema.String,
    sha256: Schema.String,
    bytes: Schema.Number,
    format: Schema.Literal("docx"),
    ooxmlNamespace: optional(Schema.String),
  }),

  metadata: Schema.Struct({
    title: optional(Schema.String),
    creator: optional(Schema.String),
    subject: optional(Schema.String),
    description: optional(Schema.String),
    keywords: optional(Schema.String),
    language: optional(Schema.String),
    created: optional(Schema.String),
    modified: optional(Schema.String),
    revision: optional(Schema.String),
    application: optional(Schema.String),
    appVersion: optional(Schema.String),
    templateName: optional(Schema.String),
    company: optional(Schema.String),
    pages: optional(Schema.Number),
    words: optional(Schema.Number),
    characters: optional(Schema.Number),
  }),

  parts: Schema.Array(Schema.String),

  document: Schema.Struct({
    sha256: Schema.String,
    sections: Schema.Array(SectionSchema),
    blocks: Schema.Array(BlockSchema),
    placeholders: Schema.Array(PlaceholderSchema),
    fields: Schema.Array(FieldSchema),
    tables: Schema.Array(TableSummarySchema),
    images: Schema.Array(ImageSummarySchema),
    numbering: Schema.Array(NumberingDefSchema),
    headers: Schema.Array(PartRefSchema),
    footers: Schema.Array(PartRefSchema),
    footnotes: optional(NoteSummarySchema),
    endnotes: optional(NoteSummarySchema),
    comments: optional(NoteSummarySchema),
  }),

  styles: Schema.Struct({
    paragraph: Schema.Array(StyleSummarySchema),
    character: Schema.Array(StyleSummarySchema),
    table: Schema.Array(StyleSummarySchema),
    latentStyleCount: optional(Schema.Number),
    defaultParagraph: optional(Schema.String),
    defaultRun: optional(Schema.String),
  }),

  /** Sorted unique placeholder tokens that MUST survive any transformation. */
  requiredPlaceholders: Schema.Array(Schema.String),
});

export type Manifest = Schema.Schema.Type<typeof ManifestSchema>;
export type Block = Schema.Schema.Type<typeof BlockSchema>;
export type Section = Schema.Schema.Type<typeof SectionSchema>;
