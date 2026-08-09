//! Deterministic project packaging. This crate owns archive layout only; reference numbering,
//! RIS generation, PDF attachment checks, and filename sanitisation come from
//! `nisaba-references`.

use std::collections::BTreeMap;
use std::fmt;
use std::io::{self, Write};

use nisaba_references::{
    Bibliography, ExportFile, ExportManifest, ExportTree, ReferenceError, ValidationError,
    validate_export,
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
    pub no_watermark: bool,
    pub not_protected: bool,
    pub commentable: bool,
    pub text_extractable: bool,
    pub indexes_rendered: bool,
    pub links_live: bool,
}

/// A report explaining why an archive can or cannot be created.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ComplianceReport {
    pub errors: Vec<String>,
}
impl ComplianceReport {
    #[must_use]
    pub fn is_compliant(&self) -> bool {
        self.errors.is_empty()
    }
}

/// Final deterministic archive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectArchive {
    pub files: Vec<ExportFile>,
    pub pdf_filename: String,
    pub report: ComplianceReport,
}

/// Export failure.
#[derive(Debug)]
pub enum ExportError {
    Blocked(ComplianceReport),
    References(Vec<ValidationError>),
    Reference(ReferenceError),
    Io(io::Error),
}
impl fmt::Display for ExportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Blocked(report) => write!(f, "export blocked: {:?}", report.errors),
            Self::References(errors) => write!(f, "reference validation failed: {errors:?}"),
            Self::Reference(error) => write!(f, "reference export failed: {error}"),
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

/// Build an archive after validating the compiled PDF and each reference tree.
pub fn build_project_archive(
    input: &ProjectArchiveInput,
    pdf: &PdfCompliance,
) -> Result<ProjectArchive, ExportError> {
    let mut report = ComplianceReport::default();
    if input.pdf.len() < 5 || !input.pdf.starts_with(b"%PDF-") {
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

    let pdf_filename = project_pdf_filename(&input.date, &input.name);
    let mut files = vec![ExportFile {
        path: pdf_filename.clone(),
        contents: input.pdf.clone(),
    }];
    for (path, source) in &input.documents {
        let safe = safe_path(path).ok_or_else(|| {
            ExportError::Blocked(ComplianceReport {
                errors: vec![format!("unsafe document path {path}")],
            })
        })?;
        files.push(ExportFile {
            path: format!("documents/{safe}"),
            contents: source.as_bytes().to_vec(),
        });
    }
    for bibliography in &input.bibliographies {
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

fn safe_component(value: &str) -> String {
    let mut out: String = value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    while out.contains("..") {
        out = out.replace("..", "_");
    }
    let out = out.trim_matches(['.', '_']);
    if out.is_empty() {
        "unnamed".into()
    } else {
        out.into()
    }
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
    Ok(writer.finish())
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
    #[allow(clippy::unnecessary_wraps, clippy::cast_possible_truncation)]
    fn file(&mut self, name: &str, data: &[u8]) -> io::Result<()> {
        let name = name.as_bytes();
        let crc = crc32(data);
        let offset = self.bytes.len() as u32;
        write_u32(&mut self.bytes, 0x0403_4b50);
        write_u16(&mut self.bytes, 20);
        write_u16(&mut self.bytes, 0);
        write_u16(&mut self.bytes, 0);
        write_u16(&mut self.bytes, 0);
        write_u16(&mut self.bytes, 0);
        write_u32(&mut self.bytes, crc);
        write_u32(&mut self.bytes, data.len() as u32);
        write_u32(&mut self.bytes, data.len() as u32);
        write_u16(&mut self.bytes, name.len() as u16);
        write_u16(&mut self.bytes, 0);
        self.bytes.extend_from_slice(name);
        self.bytes.extend_from_slice(data);
        self.entries.push((
            String::from_utf8_lossy(name).into_owned(),
            crc,
            data.len() as u32,
            offset,
        ));
        Ok(())
    }
    #[allow(clippy::cast_possible_truncation)]
    fn finish(mut self) -> Vec<u8> {
        let central = self.bytes.len() as u32;
        for (name, crc, size, offset) in &self.entries {
            let name = name.as_bytes();
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
            write_u16(&mut self.bytes, name.len() as u16);
            write_u16(&mut self.bytes, 0);
            write_u16(&mut self.bytes, 0);
            write_u16(&mut self.bytes, 0);
            write_u16(&mut self.bytes, 0);
            write_u32(&mut self.bytes, 0);
            write_u32(&mut self.bytes, *offset);
            self.bytes.extend_from_slice(name);
        }
        let central_size = self.bytes.len() as u32 - central;
        write_u32(&mut self.bytes, 0x0605_4b50);
        write_u16(&mut self.bytes, 0);
        write_u16(&mut self.bytes, 0);
        write_u16(&mut self.bytes, self.entries.len() as u16);
        write_u16(&mut self.bytes, self.entries.len() as u16);
        write_u32(&mut self.bytes, central_size);
        write_u32(&mut self.bytes, central);
        write_u16(&mut self.bytes, 0);
        self.bytes
    }
}
fn write_u16(v: &mut Vec<u8>, n: u16) {
    v.extend_from_slice(&n.to_le_bytes());
}
fn write_u32(v: &mut Vec<u8>, n: u32) {
    v.extend_from_slice(&n.to_le_bytes());
}
fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in data {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// Export-tree adapter for callers that already have a stream writer.
pub struct TreeWriter<W>(pub W);
impl<W: Write> ExportTree for TreeWriter<W> {
    type Error = io::Error;
    fn write_file(&mut self, path: &str, contents: &[u8]) -> Result<(), Self::Error> {
        self.0.write_all(path.as_bytes())?;
        self.0.write_all(b"\0")?;
        self.0.write_all(contents)
    }
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
    fn invalid_pdf_blocks_export() {
        let error =
            build_project_archive(&ProjectArchiveInput::default(), &compatible()).unwrap_err();
        assert!(matches!(error, ExportError::Blocked(_)));
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
}
