//! Deterministic project packaging. This crate owns archive layout only; reference numbering,
//! RIS generation, PDF attachment checks, and filename sanitisation come from
//! `nisaba-references`.

use std::collections::BTreeMap;
use std::fmt;
use std::io;

use nisaba_references::{
    Bibliography, ExportFile, ExportManifest, ReferenceError, ValidationError,
    safe_filename_component, validate_export,
};

/// Inputs needed to create a portable project archive.
#[derive(Clone, Debug, Default)]
pub struct ProjectArchiveInput {
    /// ISO date used in the compiled PDF filename.
    pub date: String,
    /// Human-readable project name.
    pub name: String,
    /// Compiled PDF bytes.
    pub pdf: Vec<u8>,
    /// Projected document sources, keyed by project-relative path.
    pub documents: BTreeMap<String, String>,
    /// Bibliographies attached to documents in the project.
    pub bibliographies: Vec<Bibliography>,
}

/// PDF checks supplied by the compile/compatibility boundary.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct PdfCompliance {
    /// No watermark is present.
    pub no_watermark: bool,
    /// The document is not password-protected.
    pub not_protected: bool,
    /// Annotations/comments are possible.
    pub commentable: bool,
    /// Text can be extracted (search/index requirement).
    pub text_extractable: bool,
    /// Indexes have been rendered.
    pub indexes_rendered: bool,
    /// Hyperlinks are live.
    pub links_live: bool,
}

/// A report explaining why an archive can or cannot be created.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ComplianceReport {
    /// The individual failure reasons; empty means compliant.
    pub errors: Vec<String>,
}
impl ComplianceReport {
    /// Whether the report carries no errors.
    #[must_use]
    pub fn is_compliant(&self) -> bool {
        self.errors.is_empty()
    }
}

/// Final deterministic archive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectArchive {
    /// Logical files of the archive, sorted by path.
    pub files: Vec<ExportFile>,
    /// Name the compiled PDF was stored under.
    pub pdf_filename: String,
    /// Compliance details. Always empty on success: failures return early as
    /// [`ExportError::Blocked`], which carries the report with its reasons, so a
    /// successfully built archive never has anything to report here. The field exists so
    /// callers holding an archive have a uniform place to look.
    pub report: ComplianceReport,
}

/// Export failure.
#[derive(Debug)]
pub enum ExportError {
    /// The archive was blocked by a compliance failure; carries the reasons.
    Blocked(ComplianceReport),
    /// Independent validation of the generated reference tree failed; carries every
    /// finding.
    References(Vec<ValidationError>),
    /// The reference export itself failed (numbering, missing full text, ...).
    Reference(ReferenceError),
    /// The archive exceeded a structural ZIP limit: 4 GiB of entry data or offsets
    /// beyond `u32`, a central directory too large to describe, or more than
    /// 65 535 entries. The write is aborted rather than emitting a corrupt archive.
    ArchiveTooLarge,
    /// A file name exceeded the 65 535-byte ZIP name limit.
    NameTooLong(String),
    /// An I/O failure from the underlying writer.
    Io(io::Error),
}
impl fmt::Display for ExportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Blocked(report) => {
                write!(f, "export blocked: {}", report.errors.join("; "))
            }
            Self::References(errors) => {
                write!(f, "reference validation failed: {}", error_list(errors))
            }
            Self::Reference(error) => write!(f, "reference export failed: {error}"),
            Self::ArchiveTooLarge => f.write_str(
                "archive exceeds a structural ZIP limit (4 GiB of data or 65 535 entries)",
            ),
            Self::NameTooLong(name) => {
                write!(f, "file name is too long for a ZIP entry: {name}")
            }
            Self::Io(error) => error.fmt(f),
        }
    }
}
impl std::error::Error for ExportError {}
impl From<io::Error> for ExportError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

fn error_list(errors: &[ValidationError]) -> String {
    errors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("; ")
}

/// Build an archive after validating the compiled PDF and each reference tree.
///
/// This borrows its inputs and copies every payload once. Callers that will not use the
/// inputs afterwards can pass ownership to [`build_project_archive_from_owned`] instead
/// and skip those copies.
pub fn build_project_archive(
    input: &ProjectArchiveInput,
    pdf: &PdfCompliance,
) -> Result<ProjectArchive, ExportError> {
    let pdf_filename = project_pdf_filename(&input.date, &input.name);
    let mut files = vec![ExportFile {
        path: pdf_filename.clone(),
        contents: input.pdf.clone(),
    }];
    for (path, source) in &input.documents {
        files.push(ExportFile {
            path: document_entry_path(path)?,
            contents: source.as_bytes().to_vec(),
        });
    }
    finish_archive(files, pdf, pdf_filename, &input.bibliographies)
}

/// Build an archive from owned inputs, moving (not copying) the PDF bytes and document
/// sources into the archive. Validation and layout are identical to
/// [`build_project_archive`].
pub fn build_project_archive_from_owned(
    input: ProjectArchiveInput,
    pdf: &PdfCompliance,
) -> Result<ProjectArchive, ExportError> {
    let ProjectArchiveInput {
        date,
        name,
        pdf: pdf_bytes,
        documents,
        bibliographies,
    } = input;
    let pdf_filename = project_pdf_filename(&date, &name);
    let mut files = vec![ExportFile {
        path: pdf_filename.clone(),
        contents: pdf_bytes,
    }];
    for (path, source) in documents {
        files.push(ExportFile {
            path: document_entry_path(&path)?,
            contents: source.into_bytes(),
        });
    }
    finish_archive(files, pdf, pdf_filename, &bibliographies)
}

/// The sanitized archive path for one document source, or a compliance error for an
/// unsafe path.
fn document_entry_path(path: &str) -> Result<String, ExportError> {
    safe_path(path).map_or_else(
        || {
            Err(ExportError::Blocked(ComplianceReport {
                errors: vec![format!("unsafe document path {path}")],
            }))
        },
        |safe| Ok(format!("documents/{safe}")),
    )
}

/// Shared tail of both builders: the compliance gate (PDF bytes and flags), the
/// bibliography manifests, and the deterministic sort/duplicate check. `files[0]` is the
/// PDF entry both builders prepend.
fn finish_archive(
    mut files: Vec<ExportFile>,
    pdf: &PdfCompliance,
    pdf_filename: String,
    bibliographies: &[Bibliography],
) -> Result<ProjectArchive, ExportError> {
    let mut report = ComplianceReport::default();
    if files[0].contents.len() < 5 || !files[0].contents.starts_with(b"%PDF-") {
        report.errors.push("missing or invalid PDF bytes".into());
    }
    let required = [
        (pdf.no_watermark, "PDF has a watermark"),
        (pdf.not_protected, "PDF is protected"),
        (pdf.commentable, "PDF is not electronically commentable"),
        (pdf.text_extractable, "PDF text is not extractable"),
        (pdf.indexes_rendered, "PDF indexes are not rendered"),
        (pdf.links_live, "PDF links are not live"),
    ];
    for (ok, message) in required {
        if !ok {
            report.errors.push(message.into());
        }
    }
    if !report.is_compliant() {
        return Err(ExportError::Blocked(report));
    }
    for bibliography in bibliographies {
        let manifest = ExportManifest::build(bibliography).map_err(ExportError::Reference)?;
        validate_export(bibliography, &manifest.files).map_err(ExportError::References)?;
        files.extend(manifest.files);
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    if files.windows(2).any(|pair| pair[0].path == pair[1].path) {
        return Err(ExportError::Blocked(ComplianceReport {
            errors: vec!["duplicate export path".into()],
        }));
    }
    // `report` is provably empty here (failures returned early above); the field stays
    // empty on success so callers never need to inspect it.
    Ok(ProjectArchive {
        files,
        pdf_filename,
        report,
    })
}

/// Generate a portable filename from a date and project name.
#[must_use]
pub fn project_pdf_filename(date: &str, name: &str) -> String {
    format!("{}_{}.pdf", safe_component(date), safe_component(name))
}

/// One filename component via the shared `nisaba-references` sanitization rule (see
/// [`safe_filename_component`]), so archive names fold non-ASCII letters exactly like
/// reference filenames do. Empty components become `unnamed`.
fn safe_component(value: &str) -> String {
    safe_filename_component(value, "unnamed")
}

fn safe_path(value: &str) -> Option<String> {
    if value.is_empty() || value.starts_with('/') || value.contains('\\') {
        return None;
    }
    let mut parts = Vec::new();
    for part in value.split('/') {
        if part.is_empty() || part == "." || part == ".." || part.contains('\0') {
            return None;
        }
        parts.push(safe_component(part));
    }
    Some(parts.join("/"))
}

/// Write the export as a byte-stable ZIP. Files are sorted, stored without compression,
/// and use DOS epoch timestamps, so repeated calls produce identical bytes.
///
/// Structural ZIP limits are enforced with errors, not silent truncation: data and
/// offsets beyond `u32`, more than 65 535 entries, or names beyond 65 535 bytes abort
/// the write (see [`ExportError::ArchiveTooLarge`] and [`ExportError::NameTooLong`]).
pub fn write_zip(export: &ProjectArchive) -> Result<Vec<u8>, ExportError> {
    let mut writer = ZipWriter::new();
    for file in &export.files {
        if safe_path(&file.path).is_none() {
            return Err(ExportError::Blocked(ComplianceReport {
                errors: vec![format!("unsafe ZIP path {}", file.path)],
            }));
        }
        writer.file(&file.path, &file.contents)?;
    }
    writer.finish()
}

struct ZipWriter {
    bytes: Vec<u8>,
    entries: Vec<(String, u32, u32, u32)>,
}
impl ZipWriter {
    fn new() -> Self {
        Self {
            bytes: Vec::new(),
            entries: Vec::new(),
        }
    }

    fn file(&mut self, name: &str, data: &[u8]) -> Result<(), ExportError> {
        let name_bytes = name.as_bytes();
        let name_len =
            u16::try_from(name_bytes.len()).map_err(|_| ExportError::NameTooLong(name.into()))?;
        let size = u32::try_from(data.len()).map_err(|_| ExportError::ArchiveTooLarge)?;
        let offset = u32::try_from(self.bytes.len()).map_err(|_| ExportError::ArchiveTooLarge)?;
        let crc = crc32(data);
        write_u32(&mut self.bytes, 0x0403_4b50);
        write_u16(&mut self.bytes, 20);
        write_u16(&mut self.bytes, 0);
        write_u16(&mut self.bytes, 0);
        write_u16(&mut self.bytes, 0);
        write_u16(&mut self.bytes, 0);
        write_u32(&mut self.bytes, crc);
        write_u32(&mut self.bytes, size);
        write_u32(&mut self.bytes, size);
        write_u16(&mut self.bytes, name_len);
        write_u16(&mut self.bytes, 0);
        self.bytes.extend_from_slice(name_bytes);
        self.bytes.extend_from_slice(data);
        self.entries.push((name.to_owned(), crc, size, offset));
        Ok(())
    }

    fn finish(mut self) -> Result<Vec<u8>, ExportError> {
        let central = u32::try_from(self.bytes.len()).map_err(|_| ExportError::ArchiveTooLarge)?;
        let entry_count =
            u16::try_from(self.entries.len()).map_err(|_| ExportError::ArchiveTooLarge)?;
        for (name, crc, size, offset) in &self.entries {
            let name_bytes = name.as_bytes();
            let name_len = u16::try_from(name_bytes.len())
                .map_err(|_| ExportError::NameTooLong(name.clone()))?;
            write_u32(&mut self.bytes, 0x0201_4b50);
            write_u16(&mut self.bytes, 20);
            write_u16(&mut self.bytes, 20);
            write_u16(&mut self.bytes, 0);
            write_u16(&mut self.bytes, 0);
            write_u16(&mut self.bytes, 0);
            write_u16(&mut self.bytes, 0);
            write_u32(&mut self.bytes, *crc);
            write_u32(&mut self.bytes, *size);
            write_u32(&mut self.bytes, *size);
            write_u16(&mut self.bytes, name_len);
            write_u16(&mut self.bytes, 0);
            write_u16(&mut self.bytes, 0);
            write_u16(&mut self.bytes, 0);
            write_u16(&mut self.bytes, 0);
            write_u32(&mut self.bytes, 0);
            write_u32(&mut self.bytes, *offset);
            self.bytes.extend_from_slice(name_bytes);
        }
        let central_end =
            u32::try_from(self.bytes.len()).map_err(|_| ExportError::ArchiveTooLarge)?;
        let central_size = central_end - central;
        write_u32(&mut self.bytes, 0x0605_4b50);
        write_u16(&mut self.bytes, 0);
        write_u16(&mut self.bytes, 0);
        write_u16(&mut self.bytes, entry_count);
        write_u16(&mut self.bytes, entry_count);
        write_u32(&mut self.bytes, central_size);
        write_u32(&mut self.bytes, central);
        write_u16(&mut self.bytes, 0);
        Ok(self.bytes)
    }
}
fn write_u16(v: &mut Vec<u8>, n: u16) {
    v.extend_from_slice(&n.to_le_bytes());
}
fn write_u32(v: &mut Vec<u8>, n: u32) {
    v.extend_from_slice(&n.to_le_bytes());
}

/// CRC-32 (IEEE 802.3, reflected) lookup table, computed once on first use. This
/// replaces a bit-by-bit implementation (8 iterations per byte) with the classic
/// table-driven one (one lookup per byte).
fn crc32_table() -> &'static [u32; 256] {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = [0u32; 256];
        for (i, slot) in table.iter_mut().enumerate() {
            let mut crc = u32::try_from(i).expect("index below 256");
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    (crc >> 1) ^ 0xedb8_8320
                } else {
                    crc >> 1
                };
            }
            *slot = crc;
        }
        table
    })
}

fn crc32(data: &[u8]) -> u32 {
    let table = crc32_table();
    let mut crc = !0u32;
    for &byte in data {
        let index = (crc ^ u32::from(byte)) & 0xFF;
        crc = (crc >> 8) ^ table[usize::try_from(index).expect("masked to 8 bits")];
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compatible() -> PdfCompliance {
        PdfCompliance {
            no_watermark: true,
            not_protected: true,
            commentable: true,
            text_extractable: true,
            indexes_rendered: true,
            links_live: true,
        }
    }

    #[test]
    fn names_and_paths_are_safe() {
        assert_eq!(
            project_pdf_filename("2026-04-08", "My Project"),
            "2026-04-08_My_Project.pdf"
        );
        assert!(safe_path("../evil").is_none());
        assert!(safe_path("/absolute").is_none());
    }

    #[test]
    fn non_ascii_names_fold_like_reference_filenames() {
        // Sanitization is shared with nisaba-references: European letters fold to ASCII
        // instead of collapsing to `_`, so the same name yields the same component
        // everywhere in an archive.
        assert_eq!(safe_component("Müller"), "Mueller");
        assert_eq!(
            project_pdf_filename("2026-04-08", "Müller"),
            "2026-04-08_Mueller.pdf"
        );
        assert_eq!(safe_component(""), "unnamed");
    }

    #[test]
    fn invalid_pdf_blocks_export() {
        let error =
            build_project_archive(&ProjectArchiveInput::default(), &compatible()).unwrap_err();
        assert!(matches!(error, ExportError::Blocked(_)));
    }

    #[test]
    fn owned_builder_matches_borrowed_builder() {
        let mut input = ProjectArchiveInput {
            date: "2026-04-08".into(),
            name: "Study".into(),
            pdf: b"%PDF-1.7 rest".to_vec(),
            ..ProjectArchiveInput::default()
        };
        input.documents.insert("a.typ".into(), "= A".into());
        let borrowed = build_project_archive(&input, &compatible()).unwrap();
        let owned = build_project_archive_from_owned(input, &compatible()).unwrap();
        assert_eq!(borrowed, owned);
    }

    #[test]
    fn path_traversal_is_rejected_by_archive_and_zip_seams() {
        let input = ProjectArchiveInput {
            pdf: b"%PDF-1.7".to_vec(),
            documents: [("../escape".into(), "safe".into())].into_iter().collect(),
            ..ProjectArchiveInput::default()
        };
        assert!(matches!(
            build_project_archive(&input, &compatible()),
            Err(ExportError::Blocked(_))
        ));
        let archive = ProjectArchive {
            files: vec![ExportFile {
                path: "../escape".into(),
                contents: vec![],
            }],
            pdf_filename: String::new(),
            report: ComplianceReport::default(),
        };
        assert!(matches!(write_zip(&archive), Err(ExportError::Blocked(_))));
    }

    #[test]
    fn zip_is_deterministic() {
        let archive = ProjectArchive {
            files: vec![ExportFile {
                path: "a.txt".into(),
                contents: b"hello".to_vec(),
            }],
            pdf_filename: String::new(),
            report: ComplianceReport::default(),
        };
        assert_eq!(write_zip(&archive).unwrap(), write_zip(&archive).unwrap());
    }

    #[test]
    fn crc32_matches_reference_vectors() {
        // Standard CRC-32 check value, the empty input, and every byte value.
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
        assert_eq!(crc32(b""), 0);
        let all_bytes: Vec<u8> = (0..=u8::MAX).collect();
        assert_eq!(crc32(&all_bytes), 0x2905_8c73);
    }

    #[test]
    fn oversized_name_is_rejected_not_truncated() {
        let archive = ProjectArchive {
            files: vec![ExportFile {
                path: "x".repeat(70_000),
                contents: b"hello".to_vec(),
            }],
            pdf_filename: String::new(),
            report: ComplianceReport::default(),
        };
        assert!(matches!(
            write_zip(&archive),
            Err(ExportError::NameTooLong(_))
        ));
    }

    #[test]
    fn too_many_entries_is_rejected_not_truncated() {
        // One entry over the u16 entry-count limit must error, not wrap around.
        let files: Vec<ExportFile> = (0..=u16::MAX)
            .map(|i| ExportFile {
                path: format!("f{i}.txt"),
                contents: vec![u8::try_from(i % 256).unwrap()],
            })
            .collect();
        let archive = ProjectArchive {
            files,
            pdf_filename: String::new(),
            report: ComplianceReport::default(),
        };
        assert!(matches!(
            write_zip(&archive),
            Err(ExportError::ArchiveTooLarge)
        ));
    }

    /// Walks a produced ZIP by hand (no zip dependency): local file headers, the
    /// central directory, and the end-of-central-directory record must agree on names,
    /// sizes, CRCs, and offsets, and the recorded data must match the input files.
    #[test]
    fn produced_zip_is_structurally_valid() {
        let files = vec![
            ExportFile {
                path: "dir/a.bin".into(),
                contents: vec![0, 1, 2, 0xde, 0xad, 0xff],
            },
            ExportFile {
                path: "b.txt".into(),
                contents: b"hello world".to_vec(),
            },
        ];
        let archive = ProjectArchive {
            files: files.clone(),
            pdf_filename: String::new(),
            report: ComplianceReport::default(),
        };
        let zip = write_zip(&archive).unwrap();

        let le16 = |p: usize| u16::from_le_bytes([zip[p], zip[p + 1]]);
        let le32 = |p: usize| u32::from_le_bytes([zip[p], zip[p + 1], zip[p + 2], zip[p + 3]]);

        // End of central directory: the fixed 22-byte record at the very end.
        let eocd = zip.len() - 22;
        assert_eq!(le32(eocd), 0x0605_4b50, "EOCD signature");
        let count = usize::from(le16(eocd + 10));
        let central_size = usize::try_from(le32(eocd + 12)).unwrap();
        let central_off = usize::try_from(le32(eocd + 16)).unwrap();
        assert_eq!(count, files.len());
        assert_eq!(
            central_off + central_size,
            eocd,
            "central directory must end where the EOCD begins"
        );

        let mut cursor = central_off;
        let mut seen: Vec<(String, usize)> = Vec::new();
        for _ in 0..count {
            assert_eq!(le32(cursor), 0x0201_4b50, "central header signature");
            let crc = le32(cursor + 16);
            let size = usize::try_from(le32(cursor + 20)).unwrap();
            let name_len = usize::from(le16(cursor + 28));
            let extra_len = usize::from(le16(cursor + 30));
            let comment_len = usize::from(le16(cursor + 32));
            let local_off = usize::try_from(le32(cursor + 42)).unwrap();
            let name =
                String::from_utf8(zip[cursor + 46..cursor + 46 + name_len].to_vec()).unwrap();

            // The local header at the recorded offset agrees with the central record.
            assert_eq!(le32(local_off), 0x0403_4b50, "local header signature");
            assert_eq!(le32(local_off + 14), crc, "local CRC for {name}");
            assert_eq!(le32(local_off + 18), le32(cursor + 20), "compressed size");
            assert_eq!(le32(local_off + 22), le32(cursor + 24), "uncompressed size");
            assert_eq!(le16(local_off + 28), 0, "no local extra");
            let local_name_len = usize::from(le16(local_off + 26));
            assert_eq!(
                &zip[local_off + 30..local_off + 30 + local_name_len],
                name.as_bytes()
            );

            // Stored (uncompressed) data matches the input file and its CRC.
            let data_start = local_off + 30 + local_name_len;
            let expected = &files
                .iter()
                .find(|f| f.path == name)
                .unwrap_or_else(|| panic!("unexpected entry {name}"))
                .contents;
            assert_eq!(&zip[data_start..data_start + size], expected.as_slice());
            assert_eq!(crc32(&zip[data_start..data_start + size]), crc);

            seen.push((name, size));
            cursor += 46 + name_len + extra_len + comment_len;
        }
        assert_eq!(cursor, eocd, "central directory consumed exactly");
        // Entries appear exactly in the order of `archive.files` (the builder sorts
        // them; the writer must not reorder).
        assert_eq!(
            seen.iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn error_display_is_readable() {
        let blocked = ExportError::Blocked(ComplianceReport {
            errors: vec!["PDF has a watermark".into(), "PDF is protected".into()],
        });
        assert_eq!(
            blocked.to_string(),
            "export blocked: PDF has a watermark; PDF is protected"
        );
        assert_eq!(
            ExportError::NameTooLong("big".into()).to_string(),
            "file name is too long for a ZIP entry: big"
        );
    }
}
