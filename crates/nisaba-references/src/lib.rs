//! Reference management and portable export for Nisaba.
//!
//! The crate deliberately keeps citation numbers out of the domain model.  A number is
//! an attribute of one particular bibliography build, while [`ReferenceId`] is the stable
//! identity used by attachments and citations.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt::{self, Display, Formatter, Write as _};
use std::io;

/// An opaque, stable reference identity.  It must not be made from a bibliography number.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ReferenceId(String);

impl ReferenceId {
    /// Creates an id, rejecting empty ids and ids containing path separators.
    pub fn new(value: impl Into<String>) -> Result<Self, ReferenceError> {
        let value = value.into();
        if value.is_empty() || value.contains('/') || value.contains('\\') {
            return Err(ReferenceError::InvalidId(value));
        }
        Ok(Self(value))
    }

    /// Returns the identifier's stable string representation.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for ReferenceId {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl From<ReferenceId> for String {
    fn from(id: ReferenceId) -> Self {
        id.0
    }
}

/// A CSL-like person name.  `literal` is used for corporate authors.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Person {
    pub family: Option<String>,
    pub given: Option<String>,
    pub suffix: Option<String>,
    pub literal: Option<String>,
}

impl Person {
    pub fn family(name: impl Into<String>) -> Self {
        Self {
            family: Some(name.into()),
            ..Self::default()
        }
    }

    fn display_name(&self) -> String {
        if let Some(name) = &self.literal {
            return name.clone();
        }
        match (&self.family, &self.given) {
            (Some(family), Some(given)) => format!("{family}, {given}"),
            (Some(family), None) => family.clone(),
            (None, Some(given)) => given.clone(),
            (None, None) => "UnknownAuthor".to_owned(),
        }
    }
}

/// A year, or a more precise CSL date when RIS supplied one.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct IssuedDate {
    pub year: i32,
    pub month: Option<u8>,
    pub day: Option<u8>,
}

/// Metadata shared by RIS, CSL and bibliography rendering.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Metadata {
    pub title: String,
    pub authors: Vec<Person>,
    pub issued: Option<IssuedDate>,
    pub container_title: Option<String>,
    pub publisher: Option<String>,
    pub volume: Option<String>,
    pub issue: Option<String>,
    pub pages: Option<String>,
    pub doi: Option<String>,
    pub pmid: Option<String>,
    pub url: Option<String>,
    pub abstract_text: Option<String>,
    pub keywords: Vec<String>,
    pub language: Option<String>,
    /// CSL item type (`article-journal`, `book`, `report`, ...).
    pub item_type: Option<String>,
}

/// A reference as stored in the project library.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReferenceEntry {
    pub id: ReferenceId,
    pub metadata: Metadata,
    /// RIS tags not interpreted by the CSL-ish model.  Their order and repetitions survive
    /// import/export, which is important for vendor-specific custom fields.
    pub unknown_ris: Vec<RisTag>,
    pub fulltext: Option<FullText>,
    pub provenance: Vec<Provenance>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FullText {
    pub blob_ref: String,
    pub media_type: String,
    /// The bytes supplied by the blob layer at export time. Keeping this explicit prevents
    /// a blob key from accidentally being emitted as a PDF body.
    pub contents: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Provenance {
    pub search: String,
    pub database: String,
    pub date: String,
}

/// A raw RIS tag (including the `TY`/`ER` records when requested by a parser).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RisTag {
    pub tag: String,
    pub value: String,
}

/// One parsed RIS record.  Keeping this intermediate representation makes round trips
/// possible even when a tag has no CSL equivalent.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RisRecord {
    pub tags: Vec<RisTag>,
}

impl RisRecord {
    pub fn parse(input: &str) -> Result<Vec<Self>, ReferenceError> {
        let mut records = Vec::new();
        let mut current = Vec::new();
        let mut last_tag: Option<usize> = None;
        for (line_no, line) in input.lines().enumerate() {
            let line = line.strip_suffix('\r').unwrap_or(line);
            if line.trim().is_empty() {
                continue;
            }
            let (tag, rest) = line.split_once(char::is_whitespace).ok_or_else(|| {
                ReferenceError::MalformedRis {
                    line: line_no + 1,
                    text: line.to_owned(),
                }
            })?;
            let value = rest.trim_start().strip_prefix('-').ok_or_else(|| {
                ReferenceError::MalformedRis {
                    line: line_no + 1,
                    text: line.to_owned(),
                }
            })?;
            if tag.is_empty() || !tag.chars().all(|c| c.is_ascii_alphanumeric()) {
                return Err(ReferenceError::MalformedRis {
                    line: line_no + 1,
                    text: line.to_owned(),
                });
            }
            let tag = tag.to_ascii_uppercase();
            if tag == "ER" {
                current.push(RisTag {
                    tag,
                    value: value.trim().to_owned(),
                });
                records.push(Self {
                    tags: std::mem::take(&mut current),
                });
                last_tag = None;
            } else if tag == "TY" || !current.is_empty() {
                current.push(RisTag {
                    tag,
                    value: value.trim().to_owned(),
                });
                last_tag = Some(current.len() - 1);
            } else if let Some(index) = last_tag {
                // Some EndNote exports wrap long values without repeating the tag.
                current[index].value.push(' ');
                current[index].value.push_str(value.trim());
            } else {
                return Err(ReferenceError::MalformedRis {
                    line: line_no + 1,
                    text: line.to_owned(),
                });
            }
        }
        if !current.is_empty() {
            // A missing ER is common in hand-edited RIS, but accepting it would make a
            // partial final record indistinguishable from a complete one.
            return Err(ReferenceError::MissingRecordTerminator);
        }
        Ok(records)
    }

    pub fn write_all(records: &[Self]) -> Result<String, ReferenceError> {
        let mut out = String::new();
        for record in records {
            if !record.tags.iter().any(|tag| tag.tag == "TY") {
                return Err(ReferenceError::MissingType);
            }
            for tag in &record.tags {
                if tag.tag == "ER" {
                    continue;
                }
                out.push_str(&tag.tag);
                out.push_str("  - ");
                out.push_str(&tag.value.replace('\n', " "));
                out.push('\n');
            }
            out.push_str("ER  -\n\n");
        }
        Ok(out)
    }
}

/// Parses RIS into library entries.  If `id` is absent, `AN` is used; otherwise a stable
/// deterministic id is made from the metadata (never from citation numbering).
pub fn import_ris(input: &str) -> Result<Vec<ReferenceEntry>, ReferenceError> {
    RisRecord::parse(input)?
        .into_iter()
        .enumerate()
        .map(|(index, record)| {
            let metadata = metadata_from_ris(&record)?;
            let provenance = record
                .tags
                .iter()
                .filter(|tag| tag.tag == "N1")
                .filter_map(|tag| parse_provenance(&tag.value))
                .collect();
            let id = record
                .tags
                .iter()
                .find(|tag| tag.tag == "AN")
                .map(|tag| tag.value.clone())
                .or_else(|| metadata.doi.clone().map(|doi| format!("doi:{doi}")))
                .unwrap_or_else(|| {
                    format!("ref-{:08x}", stable_hash(&dedup_title(&metadata.title)))
                });
            let id =
                ReferenceId::new(id).or_else(|_| ReferenceId::new(format!("import-{index}")))?;
            let known: HashSet<&str> = [
                "TY", "ER", "AU", "A1", "TI", "T1", "T2", "JF", "JO", "PY", "Y1", "DA", "DP", "VL",
                "IS", "SP", "EP", "DO", "UR", "AN", "RN", "AB", "LA", "PB", "KW", "PMID",
            ]
            .into_iter()
            .collect();
            Ok(ReferenceEntry {
                id,
                metadata,
                unknown_ris: record
                    .tags
                    .into_iter()
                    .filter(|tag| !known.contains(tag.tag.as_str()))
                    .collect(),
                fulltext: None,
                provenance,
            })
        })
        .collect()
}

fn metadata_from_ris(record: &RisRecord) -> Result<Metadata, ReferenceError> {
    let get = |names: &[&str]| {
        record
            .tags
            .iter()
            .find(|tag| names.contains(&tag.tag.as_str()))
            .map(|tag| tag.value.clone())
    };
    let title = get(&["TI", "T1"]).ok_or(ReferenceError::MissingTitle)?;
    let authors = record
        .tags
        .iter()
        .filter(|tag| tag.tag == "AU" || tag.tag == "A1")
        .map(|tag| parse_person(&tag.value))
        .collect();
    let issued = get(&["PY", "Y1", "DA", "DP"]).and_then(|date| parse_date(&date));
    let pages = match (get(&["SP"]), get(&["EP"])) {
        (Some(start), Some(end)) => Some(format!("{start}-{end}")),
        (Some(start), None) => Some(start),
        (None, Some(end)) => Some(end),
        _ => None,
    };
    Ok(Metadata {
        title,
        authors,
        issued,
        container_title: get(&["T2", "JF", "JO"]),
        publisher: get(&["PB"]),
        volume: get(&["VL"]),
        issue: get(&["IS"]),
        pages,
        doi: get(&["DO"]).map(|v| normalize_doi(&v)),
        pmid: get(&["PMID"])
            .or_else(|| get(&["M3"]).filter(|v| v.chars().all(|c| c.is_ascii_digit()))),
        url: get(&["UR"]),
        abstract_text: get(&["AB"]),
        keywords: record
            .tags
            .iter()
            .filter(|tag| tag.tag == "KW")
            .map(|tag| tag.value.clone())
            .collect(),
        language: get(&["LA"]),
        item_type: get(&["TY"]).map(|v| ris_type_to_csl(&v)),
    })
}

pub fn parse_person(value: &str) -> Person {
    if value.contains(',') {
        let mut parts = value.splitn(2, ',');
        return Person {
            family: parts
                .next()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned),
            given: parts
                .next()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned),
            ..Person::default()
        };
    }
    let mut words = value.split_whitespace().collect::<Vec<_>>();
    let family = words.pop().map(str::to_owned);
    Person {
        family,
        given: (!words.is_empty()).then(|| words.join(" ")),
        ..Person::default()
    }
}

fn parse_provenance(value: &str) -> Option<Provenance> {
    let mut search = None;
    let mut database = None;
    let mut date = None;
    for part in value.split(';') {
        let (key, value) = part.split_once(':')?;
        match key.trim().to_ascii_lowercase().as_str() {
            "search" => search = Some(value.trim().to_owned()),
            "database" | "db" => database = Some(value.trim().to_owned()),
            "search date" | "date" => date = Some(value.trim().to_owned()),
            _ => {}
        }
    }
    Some(Provenance {
        search: search?,
        database: database?,
        date: date?,
    })
}

fn parse_date(value: &str) -> Option<IssuedDate> {
    let parts: Vec<_> = value.split(['-', '/', '.']).collect();
    let year = parts.first()?.trim().parse().ok()?;
    Some(IssuedDate {
        year,
        month: parts.get(1).and_then(|v| v.parse().ok()),
        day: parts.get(2).and_then(|v| v.parse().ok()),
    })
}

fn ris_type_to_csl(value: &str) -> String {
    match value.to_ascii_uppercase().as_str() {
        "JOUR" | "EJOUR" => "article-journal",
        "BOOK" | "EBK" => "book",
        "RPRT" => "report",
        "CHAP" => "chapter",
        _ => "document",
    }
    .to_owned()
}

/// Emits one canonical RIS record while retaining all unmodeled source tags.
pub fn export_ris(entries: &[ReferenceEntry]) -> Result<String, ReferenceError> {
    let records = entries
        .iter()
        .map(|entry| to_ris_record(entry, None))
        .collect::<Result<Vec<_>, _>>()?;
    RisRecord::write_all(&records)
}

/// Renders entries as a hayagriva YAML bibliography so Typst's `#bibliography` can
/// resolve `@key`/`#cite(<key>)` citations.  The key is the entry's stable id
/// (a UUID string in the app) so a citation inserted from the references panel
/// resolves without a key rewrite.  Entries without a title are skipped, since
/// hayagriva requires one.  Missing fields are omitted rather than emitted blank.
/// The YAML is built by hand to avoid adding a YAML dependency to this crate;
/// every value is quoted/escaped so a title or author containing `:` or quotes
/// cannot break out of its field.
#[must_use]
pub fn bibliography_yaml(entries: &[ReferenceEntry]) -> String {
    let mut out = String::new();
    for entry in entries {
        let m = &entry.metadata;
        if m.title.trim().is_empty() {
            continue;
        }
        // Hayagriva keys must not be bare YAML booleans/nulls and must not contain
        // flow-indicators; quoting covers both. The key equals the reference id
        // verbatim so `@<id>` citations resolve without translation.
        writeln!(out, "{}:", yaml_scalar(&entry.id.to_string())).ok();
        writeln!(out, "  type: article").ok();
        writeln!(out, "  title: {}", yaml_scalar(&m.title)).ok();
        if !m.authors.is_empty() {
            out.push_str("  author:\n");
            for author in &m.authors {
                writeln!(out, "    - {}", yaml_scalar(&author.display_name())).ok();
            }
        }
        if let Some(date) = &m.issued {
            writeln!(out, "  date: {:04}", date.year.max(0)).ok();
        }
        let serials: Vec<(&str, &str)> = m
            .doi
            .as_deref()
            .map(|v| ("doi", v))
            .into_iter()
            .chain(m.pmid.as_deref().map(|v| ("pmid", v)))
            .collect();
        if !serials.is_empty() {
            out.push_str("  serial-number:\n");
            for (kind, value) in serials {
                writeln!(out, "    {kind}: {}", yaml_scalar(value)).ok();
            }
        }
        if let Some(container) = &m.container_title {
            writeln!(
                out,
                "  parent:\n    type: periodical\n    title: {}",
                yaml_scalar(container)
            )
            .ok();
        }
        if let Some(url) = &m.url {
            writeln!(out, "  url: {}", yaml_scalar(url)).ok();
        }
    }
    out
}

/// Quotes a YAML scalar double-quoted style, escaping `\` and `"`.  Used for every
/// value so titles/authors containing `:`, `#`, leading `-`, etc. stay literal.
fn yaml_scalar(value: &str) -> String {
    let escaped = value.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

fn export_numbered_ris(
    entries: &[ReferenceEntry],
    numbers: &BTreeMap<ReferenceId, u32>,
) -> Result<String, ReferenceError> {
    let records = entries
        .iter()
        .map(|entry| to_ris_record(entry, numbers.get(&entry.id).copied()))
        .collect::<Result<Vec<_>, _>>()?;
    RisRecord::write_all(&records)
}

#[allow(clippy::too_many_lines)]
fn to_ris_record(entry: &ReferenceEntry, number: Option<u32>) -> Result<RisRecord, ReferenceError> {
    let m = &entry.metadata;
    if m.title.trim().is_empty() {
        return Err(ReferenceError::MissingTitle);
    }
    let mut tags = vec![RisTag {
        tag: "TY".to_owned(),
        value: csl_type_to_ris(m.item_type.as_deref()),
    }];
    tags.extend(m.authors.iter().map(|a| RisTag {
        tag: "AU".to_owned(),
        value: a.display_name(),
    }));
    tags.push(RisTag {
        tag: "TI".to_owned(),
        value: m.title.clone(),
    });
    if let Some(v) = &m.container_title {
        tags.push(RisTag {
            tag: "T2".to_owned(),
            value: v.clone(),
        });
    }
    if let Some(v) = &m.publisher {
        tags.push(RisTag {
            tag: "PB".to_owned(),
            value: v.clone(),
        });
    }
    if let Some(v) = &m.abstract_text {
        tags.push(RisTag {
            tag: "AB".to_owned(),
            value: v.clone(),
        });
    }
    for keyword in &m.keywords {
        tags.push(RisTag {
            tag: "KW".to_owned(),
            value: keyword.clone(),
        });
    }
    if let Some(v) = &m.language {
        tags.push(RisTag {
            tag: "LA".to_owned(),
            value: v.clone(),
        });
    }
    if let Some(date) = &m.issued {
        tags.push(RisTag {
            tag: "PY".to_owned(),
            value: date.year.to_string(),
        });
    }
    for (tag, value) in [
        ("VL", &m.volume),
        ("IS", &m.issue),
        ("DO", &m.doi),
        ("PMID", &m.pmid),
        ("UR", &m.url),
    ] {
        if let Some(value) = value {
            tags.push(RisTag {
                tag: tag.to_owned(),
                value: value.clone(),
            });
        }
    }
    if let Some(pages) = &m.pages {
        let mut split = pages.splitn(2, '-');
        tags.push(RisTag {
            tag: "SP".to_owned(),
            value: split.next().unwrap_or_default().to_owned(),
        });
        if let Some(end) = split.next() {
            tags.push(RisTag {
                tag: "EP".to_owned(),
                value: end.to_owned(),
            });
        }
    }
    tags.extend(
        entry
            .unknown_ris
            .iter()
            .filter(|tag| tag.tag != "TY" && tag.tag != "ER")
            .cloned(),
    );
    if !entry.unknown_ris.iter().any(|tag| tag.tag == "N1") {
        for provenance in &entry.provenance {
            tags.push(RisTag {
                tag: "N1".to_owned(),
                value: format!(
                    "database search: {}; database: {}; search date: {}",
                    provenance.search, provenance.database, provenance.date
                ),
            });
        }
    }
    tags.push(RisTag {
        tag: "AN".to_owned(),
        value: entry.id.to_string(),
    });
    if let Some(number) = number {
        tags.push(RisTag {
            tag: "RN".to_owned(),
            value: number.to_string(),
        });
    }
    Ok(RisRecord { tags })
}

fn csl_type_to_ris(value: Option<&str>) -> String {
    match value.unwrap_or("article-journal") {
        "book" => "BOOK",
        "chapter" => "CHAP",
        "report" => "RPRT",
        _ => "JOUR",
    }
    .to_owned()
}

/// A canonical deduplication key.  DOI and PMID take precedence over title.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum DuplicateKey {
    Doi(String),
    Pmid(String),
    Title(String),
}

#[must_use]
pub fn dedup_key(metadata: &Metadata) -> DuplicateKey {
    if let Some(doi) = &metadata.doi {
        return DuplicateKey::Doi(normalize_doi(doi));
    }
    if let Some(pmid) = &metadata.pmid {
        return DuplicateKey::Pmid(normalize_pmid(pmid));
    }
    DuplicateKey::Title(dedup_title(&metadata.title))
}

/// Returns entries with duplicate records removed, retaining the first record and merging
/// unknown tags, provenance and full text from later records. Any matching DOI, PMID, or
/// normalized title is enough to merge two records (identifiers from different databases
/// need not use the same primary key).
pub fn deduplicate(entries: impl IntoIterator<Item = ReferenceEntry>) -> Vec<ReferenceEntry> {
    let mut result: Vec<ReferenceEntry> = Vec::new();
    let mut keys: HashMap<DuplicateKey, usize> = HashMap::new();
    for mut entry in entries {
        let entry_keys = dedup_keys(&entry.metadata);
        let existing = entry_keys.iter().find_map(|key| keys.get(key).copied());
        if let Some(index) = existing {
            let kept = &mut result[index];
            for tag in entry.unknown_ris.drain(..) {
                if !kept.unknown_ris.contains(&tag) {
                    kept.unknown_ris.push(tag);
                }
            }
            if kept.fulltext.is_none() {
                kept.fulltext = entry.fulltext;
            }
            kept.provenance.extend(entry.provenance);
        } else {
            let index = result.len();
            for key in entry_keys {
                keys.insert(key, index);
            }
            result.push(entry);
        }
    }
    result
}

fn dedup_keys(metadata: &Metadata) -> Vec<DuplicateKey> {
    let mut keys = vec![DuplicateKey::Title(dedup_title(&metadata.title))];
    if let Some(doi) = &metadata.doi {
        keys.push(DuplicateKey::Doi(normalize_doi(doi)));
    }
    if let Some(pmid) = &metadata.pmid {
        keys.push(DuplicateKey::Pmid(normalize_pmid(pmid)));
    }
    keys
}

#[must_use]
pub fn normalize_doi(value: &str) -> String {
    value
        .trim()
        .trim_start_matches("https://doi.org/")
        .trim_start_matches("http://doi.org/")
        .trim_start_matches("doi:")
        .trim()
        .to_ascii_lowercase()
}

#[must_use]
pub fn normalize_pmid(value: &str) -> String {
    value
        .trim()
        .trim_start_matches(|c: char| c.eq_ignore_ascii_case(&'p'))
        .trim_start_matches(|c: char| c.eq_ignore_ascii_case(&'m'))
        .trim_start_matches(|c: char| c.eq_ignore_ascii_case(&'i'))
        .trim_start_matches(|c: char| c.eq_ignore_ascii_case(&'d'))
        .trim_start_matches(':')
        .trim()
        .to_owned()
}

#[must_use]
pub fn dedup_title(value: &str) -> String {
    value
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn stable_hash(value: &str) -> u64 {
    // Fixed FNV-1a, rather than DefaultHasher whose algorithm is not a public stability promise.
    value.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// One citation occurrence extracted from Typst source.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Citation {
    pub reference_id: ReferenceId,
    /// Byte offset of the occurrence's key into the concatenated source.
    pub byte_offset: usize,
}

/// Extracts `@key` and `#cite(<key>, ...)` references.  Comments and quoted strings are
/// ignored, and keys are returned in source order (repeated citations remain occurrences).
#[allow(clippy::too_many_lines)]
pub fn extract_citations(source: &str) -> Result<Vec<Citation>, ReferenceError> {
    let bytes = source.as_bytes();
    let mut result = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            i = skip_line(bytes, i + 2);
            continue;
        }
        if bytes[i] == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            i = skip_block_comment(bytes, i + 2);
            continue;
        }
        if bytes[i] == b'"' {
            i = skip_string(bytes, i + 1, b'"');
            continue;
        }
        if bytes[i] == b'@' {
            let start = i;
            i += 1;
            let end = key_end(bytes, i);
            if end > i {
                result.push(Citation {
                    reference_id: ReferenceId::new(&source[i..end])?,
                    byte_offset: start,
                });
            }
            i = end;
            continue;
        }
        if bytes[i] == b'#'
            && source[i..].starts_with("#cite")
            && (i + 5 == bytes.len() || !bytes[i + 5].is_ascii_alphanumeric())
        {
            let mut cursor = i + 5;
            while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
                cursor += 1;
            }
            if cursor >= bytes.len() || bytes[cursor] != b'(' {
                i += 1;
                continue;
            }
            let close = matching_paren(bytes, cursor)
                .ok_or(ReferenceError::UnclosedCitation { byte_offset: i })?;
            let mut p = cursor + 1;
            while p < close {
                while p < close && (bytes[p].is_ascii_whitespace() || bytes[p] == b',') {
                    p += 1;
                }
                if p >= close {
                    break;
                }
                // Typst accepts three spellings for a cite key, and all three occur in
                // real projects: `#cite(<key>)` (label), `#cite("key")` (string), and the
                // bare `#cite(key)`. Named arguments (`form: "prose"`, `style: ...`) are
                // not keys and are skipped.
                let start = p;
                if bytes[p] == b'<' || bytes[p] == b'"' {
                    let closing = if bytes[p] == b'<' { b'>' } else { b'"' };
                    p += 1;
                    let Some(end) = bytes[p..close].iter().position(|b| *b == closing) else {
                        break;
                    };
                    let end = p + end;
                    if end > p {
                        result.push(Citation {
                            reference_id: ReferenceId::new(&source[p..end])?,
                            byte_offset: start,
                        });
                    }
                    p = end + 1;
                } else {
                    // A bare argument is a Typst identifier. Unlike a `<label>` it cannot
                    // contain `:`, so stopping there is what distinguishes the cite key
                    // `#cite(key)` from the named argument `#cite(form: "prose")`.
                    let end = identifier_end(bytes, p).min(close);
                    let mut after = end;
                    while after < close && bytes[after].is_ascii_whitespace() {
                        after += 1;
                    }
                    let named_argument = after < close && bytes[after] == b':';
                    if end > start && !named_argument {
                        result.push(Citation {
                            reference_id: ReferenceId::new(&source[start..end])?,
                            byte_offset: start,
                        });
                    }
                    if named_argument {
                        let mut value = after + 1;
                        while value < close && bytes[value].is_ascii_whitespace() {
                            value += 1;
                        }
                        p = if value < close && bytes[value] == b'"' {
                            skip_string(bytes, value + 1, b'"')
                        } else if value < close && bytes[value] == b'<' {
                            let Some(end) = bytes[value + 1..close].iter().position(|b| *b == b'>')
                            else {
                                break;
                            };
                            (value + 1 + end).min(close) + 1
                        } else {
                            value
                        };
                    } else {
                        p = if end > start { end } else { p + 1 };
                    }
                }
                while p < close && bytes[p] != b',' {
                    p += 1;
                }
            }
            i = close + 1;
            continue;
        }
        i += 1;
    }
    Ok(result)
}

fn skip_line(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && bytes[i] != b'\n' {
        i += 1;
    }
    i
}
fn skip_block_comment(bytes: &[u8], mut i: usize) -> usize {
    while i + 1 < bytes.len() {
        if bytes[i] == b'*' && bytes[i + 1] == b'/' {
            return i + 2;
        }
        i += 1;
    }
    bytes.len()
}
fn skip_string(bytes: &[u8], mut i: usize, quote: u8) -> usize {
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            i += 2;
        } else if bytes[i] == quote {
            return i + 1;
        } else {
            i += 1;
        }
    }
    bytes.len()
}
fn key_end(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || b"_-:.".contains(&bytes[i])) {
        i += 1;
    }
    i
}
/// Like [`key_end`], but stops at `:` so a named argument can be told apart from a key.
fn identifier_end(bytes: &[u8], mut i: usize) -> usize {
    while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || b"_-.".contains(&bytes[i])) {
        i += 1;
    }
    i
}
fn matching_paren(bytes: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0;
    let mut i = open;
    while i < bytes.len() {
        if bytes[i] == b'"' {
            i = skip_string(bytes, i + 1, b'"');
            continue;
        }
        if bytes[i] == b'(' {
            depth += 1;
        } else if bytes[i] == b')' {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// A bibliography number is derived from the first citation occurrence.  Uncited entries
/// are appended in stable input order.  Equal positions (possible after source merges) use
/// the stable reference id as a deterministic tie breaker.
pub fn number_bibliography(
    entries: &[ReferenceEntry],
    citations: &[Citation],
) -> Result<BTreeMap<ReferenceId, u32>, ReferenceError> {
    let known: HashSet<&ReferenceId> = entries.iter().map(|e| &e.id).collect();
    let mut first = HashMap::<&ReferenceId, usize>::new();
    for (position, citation) in citations.iter().enumerate() {
        if !known.contains(&citation.reference_id) {
            return Err(ReferenceError::UnknownCitation(
                citation.reference_id.clone(),
            ));
        }
        first
            .entry(&citation.reference_id)
            .or_insert(citation.byte_offset.min(position));
    }
    let mut ordered: Vec<&ReferenceEntry> = entries.iter().collect();
    ordered.sort_by(|a, b| {
        first
            .get(&a.id)
            .copied()
            .unwrap_or(usize::MAX)
            .cmp(&first.get(&b.id).copied().unwrap_or(usize::MAX))
            .then_with(|| a.id.cmp(&b.id))
    });
    let mut numbered = BTreeMap::new();
    for (n, entry) in ordered.into_iter().enumerate() {
        let number = u32::try_from(n)
            .map_err(|_| ReferenceError::InvalidId(entry.id.to_string()))?
            .checked_add(1)
            .ok_or_else(|| ReferenceError::InvalidId(entry.id.to_string()))?;
        numbered.insert(entry.id.clone(), number);
    }
    Ok(numbered)
}

/// Produces the mandated `number_first-author_year.pdf` name without allowing a path to be
/// escaped.  Unsupported filename characters become `_`; the number is supplied by the
/// current export and is not stored on the reference.
#[must_use]
pub fn fulltext_filename(number: u32, metadata: &Metadata) -> String {
    let author = metadata.authors.first().map_or("UnknownAuthor", |p| {
        p.family
            .as_deref()
            .or(p.literal.as_deref())
            .unwrap_or("UnknownAuthor")
    });
    let author = safe_filename_component(author);
    let year = metadata.issued.as_ref().map_or("0000".to_owned(), |date| {
        if (0..=9999).contains(&date.year) {
            format!("{:04}", date.year)
        } else {
            "0000".to_owned()
        }
    });
    format!("{number}_{author}_{year}.pdf")
}

/// Latin-1/Latin Extended-A letters that appear in European author names, folded to
/// their closest ASCII spelling so `Müller` becomes `Mueller` rather than `M_ller`.
/// Anything outside this table still falls back to `_`.
fn fold_to_ascii(c: char) -> Option<&'static str> {
    Some(match c {
        'ä' | 'æ' => "ae",
        'ö' | 'œ' => "oe",
        'ü' => "ue",
        'Ä' | 'Æ' => "Ae",
        'Ö' | 'Œ' => "Oe",
        'Ü' => "Ue",
        'ß' => "ss",
        'à' | 'á' | 'â' | 'ã' | 'å' | 'ā' | 'ă' | 'ą' => "a",
        'À' | 'Á' | 'Â' | 'Ã' | 'Å' | 'Ā' | 'Ă' | 'Ą' => "A",
        'ç' | 'ć' | 'č' => "c",
        'Ç' | 'Ć' | 'Č' => "C",
        'è' | 'é' | 'ê' | 'ë' | 'ē' | 'ė' | 'ę' | 'ě' => "e",
        'È' | 'É' | 'Ê' | 'Ë' | 'Ē' | 'Ė' | 'Ę' | 'Ě' => "E",
        'ì' | 'í' | 'î' | 'ï' | 'ī' | 'į' => "i",
        'Ì' | 'Í' | 'Î' | 'Ï' | 'Ī' | 'Į' => "I",
        'ñ' | 'ń' | 'ň' => "n",
        'Ñ' | 'Ń' | 'Ň' => "N",
        'ò' | 'ó' | 'ô' | 'õ' | 'ø' | 'ō' => "o",
        'Ò' | 'Ó' | 'Ô' | 'Õ' | 'Ø' | 'Ō' => "O",
        'ù' | 'ú' | 'û' | 'ū' | 'ů' => "u",
        'Ù' | 'Ú' | 'Û' | 'Ū' | 'Ů' => "U",
        'ý' | 'ÿ' => "y",
        'Ý' => "Y",
        'ł' => "l",
        'Ł' => "L",
        'ś' | 'š' => "s",
        'Ś' | 'Š' => "S",
        'ź' | 'ż' | 'ž' => "z",
        'Ź' | 'Ż' | 'Ž' => "Z",
        'đ' => "d",
        'Đ' => "D",
        'ť' | 'ţ' => "t",
        'Ť' | 'Ţ' => "T",
        'ř' => "r",
        'Ř' => "R",
        _ => return None,
    })
}

fn safe_filename_component(value: &str) -> String {
    let mut output = value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c.to_string()
            } else {
                fold_to_ascii(c).unwrap_or("_").to_owned()
            }
        })
        .collect::<String>();
    while output.contains("..") {
        output = output.replace("..", "_");
    }
    let output = output.trim_matches(['.', '_']).to_owned();
    if output.is_empty() {
        "UnknownAuthor".to_owned()
    } else {
        output
    }
}

/// The logical files written by an export.  A writer can map this abstraction to a zip,
/// object store, or a directory without changing numbering/validation logic.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExportFile {
    pub path: String,
    pub contents: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Bibliography {
    pub directory: String,
    pub entries: Vec<ReferenceEntry>,
    pub citations: Vec<Citation>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExportManifest {
    pub files: Vec<ExportFile>,
    pub numbers: BTreeMap<ReferenceId, u32>,
}

pub trait ExportTree {
    type Error;
    fn write_file(&mut self, path: &str, contents: &[u8]) -> Result<(), Self::Error>;
}

impl ExportManifest {
    /// Builds RIS and full-text files below the requested directory. Missing full text is an
    /// error before any files are emitted, so callers never receive a partial export.
    pub fn build(bibliography: &Bibliography) -> Result<Self, ReferenceError> {
        let numbers = number_bibliography(&bibliography.entries, &bibliography.citations)?;
        let missing: Vec<_> = bibliography
            .entries
            .iter()
            .filter(|e| {
                numbers.contains_key(&e.id)
                    && e.fulltext.is_none()
                    && bibliography
                        .citations
                        .iter()
                        .any(|c| c.reference_id == e.id)
            })
            .map(|e| e.id.clone())
            .collect();
        if !missing.is_empty() {
            return Err(ReferenceError::MissingFullText(missing));
        }
        let ris = export_numbered_ris(&bibliography.entries, &numbers)?.into_bytes();
        let prefix = safe_filename_component(&bibliography.directory);
        let mut files = vec![ExportFile {
            path: format!("{prefix}/references.ris"),
            contents: ris,
        }];
        for entry in &bibliography.entries {
            if let (Some(fulltext), Some(number)) = (&entry.fulltext, numbers.get(&entry.id)) {
                files.push(ExportFile {
                    path: format!(
                        "{prefix}/full-text/{}",
                        fulltext_filename(*number, &entry.metadata)
                    ),
                    contents: fulltext.contents.clone(),
                });
            }
        }
        files.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(Self { files, numbers })
    }

    pub fn write_to<T: ExportTree>(&self, tree: &mut T) -> Result<(), T::Error> {
        for file in &self.files {
            tree.write_file(&file.path, &file.contents)?;
        }
        Ok(())
    }
}

/// Validates a generated tree independently of its builder.  `files` is the actual tree,
/// while `ris` is the actual exported RIS bytes; this catches stale/misnamed attachments.
#[allow(clippy::too_many_lines)]
pub fn validate_export(
    bibliography: &Bibliography,
    files: &[ExportFile],
) -> Result<(), Vec<ValidationError>> {
    let mut errors = Vec::new();
    let numbers = match number_bibliography(&bibliography.entries, &bibliography.citations) {
        Ok(n) => n,
        Err(error) => {
            errors.push(ValidationError::Numbering(error));
            return Err(errors);
        }
    };
    let prefix = safe_filename_component(&bibliography.directory);
    let ris_path = format!("{prefix}/references.ris");
    let ris_file = files.iter().find(|file| file.path == ris_path);
    if ris_file.is_none() {
        errors.push(ValidationError::MissingRis(ris_path.clone()));
    }
    let exported = ris_file
        .and_then(|file| std::str::from_utf8(&file.contents).ok())
        .map(
            |content| match (import_ris(content), RisRecord::parse(content)) {
                (Ok(entries), Ok(records)) => {
                    let ids = entries.into_iter().map(|e| e.id).collect::<BTreeSet<_>>();
                    let ris_numbers = records
                        .iter()
                        .filter_map(|record| {
                            let id = record
                                .tags
                                .iter()
                                .find(|tag| tag.tag == "AN")?
                                .value
                                .clone();
                            let number = record
                                .tags
                                .iter()
                                .find(|tag| tag.tag == "RN")?
                                .value
                                .parse()
                                .ok()?;
                            Some((id, number))
                        })
                        .collect::<HashMap<_, u32>>();
                    (ids, ris_numbers)
                }
                (Err(error), _) | (_, Err(error)) => {
                    errors.push(ValidationError::InvalidRis(error));
                    (BTreeSet::new(), HashMap::new())
                }
            },
        );
    let exported_ids = exported.as_ref().map(|(ids, _)| ids);
    let exported_numbers = exported.as_ref().map(|(_, numbers)| numbers);
    for entry in &bibliography.entries {
        if let Some(number) = numbers.get(&entry.id) {
            let expected = format!(
                "{prefix}/full-text/{}",
                fulltext_filename(*number, &entry.metadata)
            );
            let citation = bibliography
                .citations
                .iter()
                .any(|c| c.reference_id == entry.id);
            if citation
                && entry
                    .fulltext
                    .as_ref()
                    .is_none_or(|fulltext| fulltext.contents.is_empty())
            {
                errors.push(ValidationError::MissingFullText(entry.id.clone()));
            }
            if exported_numbers
                .and_then(|numbers| numbers.get(entry.id.as_str()))
                .is_none()
            {
                errors.push(ValidationError::MissingRisNumber(entry.id.clone()));
            } else if exported_numbers
                .and_then(|numbers| numbers.get(entry.id.as_str()))
                .is_some_and(|ris_number| *ris_number != *number)
            {
                let actual = exported_numbers
                    .and_then(|numbers| numbers.get(entry.id.as_str()))
                    .copied()
                    .unwrap_or_default();
                errors.push(ValidationError::RisNumberMismatch {
                    id: entry.id.clone(),
                    expected: *number,
                    actual,
                });
            }
            if citation && !files.iter().any(|file| file.path == expected) {
                errors.push(ValidationError::MissingFile(expected));
            }
            if exported_ids
                .as_ref()
                .is_some_and(|ids| !ids.contains(&entry.id))
            {
                errors.push(ValidationError::RisMissingId(entry.id.clone()));
            }
        }
    }
    let expected_paths: Vec<_> = bibliography
        .entries
        .iter()
        .filter_map(|e| {
            numbers
                .get(&e.id)
                .map(|n| format!("{prefix}/full-text/{}", fulltext_filename(*n, &e.metadata)))
        })
        .collect();
    if expected_paths.iter().collect::<BTreeSet<_>>().len() != expected_paths.len() {
        errors.push(ValidationError::DuplicateFilename);
    }
    for file in files
        .iter()
        .filter(|file| file.path.starts_with(&format!("{prefix}/full-text/")))
    {
        if !expected_paths.contains(&file.path) {
            errors.push(ValidationError::UnexpectedFile(file.path.clone()));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ValidationError {
    Numbering(ReferenceError),
    MissingRis(String),
    MissingFullText(ReferenceId),
    MissingFile(String),
    RisMissingId(ReferenceId),
    UnexpectedFile(String),
    DuplicateFilename,
    InvalidRis(ReferenceError),
    RisNumberMismatch {
        id: ReferenceId,
        expected: u32,
        actual: u32,
    },
    MissingRisNumber(ReferenceId),
}

impl Display for ValidationError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReferenceError {
    InvalidId(String),
    MalformedRis { line: usize, text: String },
    MissingRecordTerminator,
    MissingType,
    MissingTitle,
    UnknownCitation(ReferenceId),
    UnclosedCitation { byte_offset: usize },
    MissingFullText(Vec<ReferenceId>),
}

impl Display for ReferenceError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for ReferenceError {}

/// A small in-memory writer useful to callers and tests.
#[derive(Default)]
pub struct MemoryTree(pub BTreeMap<String, Vec<u8>>);
impl ExportTree for MemoryTree {
    type Error = io::Error;
    fn write_file(&mut self, path: &str, contents: &[u8]) -> Result<(), Self::Error> {
        self.0.insert(path.to_owned(), contents.to_vec());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, title: &str, author: &str, year: i32) -> ReferenceEntry {
        ReferenceEntry {
            id: ReferenceId::new(id).unwrap(),
            metadata: Metadata {
                title: title.to_owned(),
                authors: vec![Person::family(author)],
                issued: Some(IssuedDate {
                    year,
                    ..IssuedDate::default()
                }),
                ..Metadata::default()
            },
            unknown_ris: Vec::new(),
            fulltext: Some(FullText {
                blob_ref: format!("blob:{id}"),
                media_type: "application/pdf".to_owned(),
                contents: b"%PDF-1.7 fixture".to_vec(),
            }),
            provenance: Vec::new(),
        }
    }

    #[test]
    fn ris_preserves_search_fields_and_unknown_tags() {
        let source = "TY  - JOUR\nAU  - Mustermann, Erika\nTI  - A document\nT2  - Journal of Testing\nPY  - 2024\nDO  - https://doi.org/10.1000/ABC\nN1  - database search: MEDLINE\nQZ  - vendor-required\nER  -\n";
        let imported = import_ris(source).unwrap();
        assert_eq!(imported[0].metadata.doi.as_deref(), Some("10.1000/abc"));
        assert_eq!(
            imported[0].metadata.authors[0].family.as_deref(),
            Some("Mustermann")
        );
        assert!(imported[0].unknown_ris.iter().any(|tag| tag.tag == "QZ"));
        let output = export_ris(&imported).unwrap();
        assert!(output.contains("QZ  - vendor-required"));
        assert!(output.contains("T2  - Journal of Testing"));
        assert!(output.contains("N1  - database search: MEDLINE"));
        let fixture = include_str!("../../../fixtures/references/endnote-export.ris");
        let fixture_entries = import_ris(fixture).unwrap();
        assert_eq!(fixture_entries[0].provenance[0].database, "MEDLINE");
    }

    #[test]
    fn ris_rejects_truncated_record() {
        assert_eq!(
            RisRecord::parse("TY  - JOUR\nTI  - x\n"),
            Err(ReferenceError::MissingRecordTerminator)
        );
    }

    #[test]
    fn dedup_uses_doi_pmid_then_normalized_title() {
        let mut a = entry("a", "A Study: of  Drugs!", "A", 2020);
        let mut b = entry("b", "a study of drugs", "B", 2021);
        assert_eq!(
            dedup_key(&a.metadata),
            DuplicateKey::Title("a study of drugs".to_owned())
        );
        assert_eq!(deduplicate(vec![a.clone(), b.clone()]).len(), 1);
        a.metadata.doi = Some("https://doi.org/10.1/XYZ".to_owned());
        b.metadata.doi = Some("doi:10.1/xyz".to_owned());
        assert_eq!(deduplicate(vec![a, b]).len(), 1);
        let endnote = import_ris(include_str!(
            "../../../fixtures/references/endnote-export.ris"
        ))
        .unwrap();
        let zotero = import_ris(include_str!(
            "../../../fixtures/references/zotero-export.ris"
        ))
        .unwrap();
        assert_eq!(endnote.len(), 2);
        assert_eq!(zotero.len(), 2);
    }

    #[test]
    fn extracts_typst_citations_in_order_and_skips_comments_strings() {
        let source = "// @wrong\nlet x = \"@wrong\"\nSee @first and #cite(<second>, <third>, form: \"prose\")";
        let citations = extract_citations(source).unwrap();
        assert_eq!(
            citations
                .iter()
                .map(|c| c.reference_id.as_str())
                .collect::<Vec<_>>(),
            vec!["first", "second", "third"]
        );
    }

    #[test]
    fn extracts_string_and_bare_cite_keys_but_not_named_arguments() {
        let source = r#"#cite("alpha") #cite(beta) #cite(<gamma>, form: "prose") #cite("delta", style: "ieee")"#;
        let citations = extract_citations(source).unwrap();
        assert_eq!(
            citations
                .iter()
                .map(|c| c.reference_id.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha", "beta", "gamma", "delta"]
        );
    }

    #[test]
    fn named_argument_value_with_comma_is_not_split_into_citations() {
        let source = r#"#cite(k0, supplement: "5th ed., vol. 2") #cite(<l1>, edition: "rev.") #cite(b2, form: <label>)"#;
        let citations = extract_citations(source).unwrap();
        assert_eq!(
            citations
                .iter()
                .map(|c| c.reference_id.as_str())
                .collect::<Vec<_>>(),
            vec!["k0", "l1", "b2"]
        );
    }

    #[test]
    fn folds_non_ascii_author_names_distinctly() {
        let mut with_u = entry("a", "t", "x", 2024);
        with_u.metadata.authors[0].family = Some("Müller".to_owned());
        let mut with_o = entry("b", "t", "x", 2024);
        with_o.metadata.authors[0].family = Some("Möller".to_owned());
        assert_eq!(fulltext_filename(1, &with_u.metadata), "1_Mueller_2024.pdf");
        assert_eq!(fulltext_filename(2, &with_o.metadata), "2_Moeller_2024.pdf");
    }

    #[test]
    fn numbering_is_first_use_then_stable_id() {
        let entries = vec![
            entry("z", "Z", "Z", 2020),
            entry("a", "A", "A", 2020),
            entry("unused", "U", "U", 2020),
        ];
        let citations = extract_citations("@z @a @z").unwrap();
        let numbers = number_bibliography(&entries, &citations).unwrap();
        assert_eq!(numbers[&ReferenceId::new("z").unwrap()], 1);
        assert_eq!(numbers[&ReferenceId::new("a").unwrap()], 2);
        assert_eq!(numbers[&ReferenceId::new("unused").unwrap()], 3);
    }

    #[test]
    fn filename_is_safe_and_number_is_not_identity() {
        let mut e = entry("stable-id", "x", "../../Müller/Smith", 2024);
        e.metadata.authors[0].family = Some("../../Müller/Smith".to_owned());
        // Path separators and dots are neutralised; European letters fold to ASCII
        // rather than collapsing to `_` (which collided across distinct authors).
        assert_eq!(
            fulltext_filename(7, &e.metadata),
            "7_Mueller_Smith_2024.pdf"
        );
        assert_ne!(e.id.as_str(), "7");
    }

    #[test]
    fn export_and_validator_cross_check_tree() {
        let entries = vec![
            entry("r1", "One", "Alpha", 2020),
            entry("r2", "Two", "Beta", 2021),
        ];
        let bibliography = Bibliography {
            directory: "references".to_owned(),
            citations: extract_citations("@r2 @r1").unwrap(),
            entries,
        };
        let manifest = ExportManifest::build(&bibliography).unwrap();
        assert!(
            validate_export(&bibliography, &manifest.files).is_ok(),
            "{:?}",
            validate_export(&bibliography, &manifest.files)
        );
        let mut broken = manifest.files.clone();
        broken.retain(|file| !file.path.contains("1_Beta"));
        assert!(validate_export(&bibliography, &broken).is_err());
    }

    #[test]
    fn renumbering_rebuilds_attachment_filenames() {
        let entries = vec![
            entry("r1", "One", "Alpha", 2020),
            entry("r2", "Two", "Beta", 2021),
        ];
        let first = Bibliography {
            directory: "references".to_owned(),
            citations: extract_citations("@r1 @r2").unwrap(),
            entries: entries.clone(),
        };
        let first_manifest = ExportManifest::build(&first).unwrap();
        assert!(
            first_manifest
                .files
                .iter()
                .any(|file| file.path.ends_with("1_Alpha_2020.pdf"))
        );
        let mut inserted = entry("new", "New", "NewAuthor", 2019);
        inserted.fulltext = Some(FullText {
            blob_ref: "blob:new".into(),
            media_type: "application/pdf".into(),
            contents: b"%PDF-new".to_vec(),
        });
        let second = Bibliography {
            directory: "references".to_owned(),
            citations: extract_citations("@new @r1 @r2").unwrap(),
            entries: [vec![inserted], entries].concat(),
        };
        let second_manifest = ExportManifest::build(&second).unwrap();
        assert!(
            second_manifest
                .files
                .iter()
                .any(|file| file.path.ends_with("2_Alpha_2020.pdf"))
        );
        assert!(
            !second_manifest
                .files
                .iter()
                .any(|file| file.path.ends_with("1_Alpha_2020.pdf"))
        );
    }

    #[test]
    fn missing_full_text_blocks_export_before_files_exist() {
        let mut e = entry("r", "One", "Alpha", 2020);
        e.fulltext = None;
        let b = Bibliography {
            directory: "references".to_owned(),
            citations: extract_citations("@r").unwrap(),
            entries: vec![e],
        };
        assert!(matches!(
            ExportManifest::build(&b),
            Err(ReferenceError::MissingFullText(_))
        ));
    }

    #[test]
    fn writer_is_an_export_seam() {
        let b = Bibliography {
            directory: "references".to_owned(),
            citations: extract_citations("@r").unwrap(),
            entries: vec![entry("r", "One", "Alpha", 2020)],
        };
        let manifest = ExportManifest::build(&b).unwrap();
        let mut tree = MemoryTree::default();
        manifest.write_to(&mut tree).unwrap();
        assert_eq!(tree.0.len(), 2);
        let ris = String::from_utf8(
            tree.0
                .values()
                .find(|bytes| bytes.windows(4).any(|w| w == b"RN  "))
                .unwrap()
                .clone(),
        )
        .unwrap();
        assert!(ris.contains("RN  - 1"));
    }

    #[test]
    fn bibliography_yaml_emits_hayagriva_keyed_by_id() {
        // The citation key the app inserts is the reference id (a UUID string),
        // so the hayagriva entry MUST be keyed by exactly that id. Authors with
        // given+family names render as "Family, Given"; date becomes a year.
        let mut e = entry(
            "11111111-2222-3333-4444-555555555555",
            "A Title",
            "Alpha",
            2024,
        );
        e.metadata.authors[0] = Person {
            family: Some("Beta".into()),
            given: Some("Bob".into()),
            ..Person::default()
        };
        e.metadata.doi = Some("10.1000/abc".into());
        e.metadata.pmid = Some("123".into());
        e.metadata.container_title = Some("Journal of Testing".into());
        e.metadata.url = Some("https://example.org/x".into());
        let yaml = bibliography_yaml(&[e]);
        assert!(yaml.contains("\"11111111-2222-3333-4444-555555555555\":"));
        assert!(yaml.contains("  type: article"));
        assert!(yaml.contains("  title: \"A Title\""));
        assert!(yaml.contains("    - \"Beta, Bob\""));
        assert!(yaml.contains("  date: 2024"));
        assert!(yaml.contains("    doi: \"10.1000/abc\""));
        assert!(yaml.contains("    pmid: \"123\""));
        assert!(yaml.contains("    type: periodical"));
        assert!(yaml.contains("    title: \"Journal of Testing\""));
        assert!(yaml.contains("  url: \"https://example.org/x\""));
    }

    #[test]
    fn bibliography_yaml_quotes_colons_and_quotes_and_skips_titleless() {
        // A title or author containing `:`, `"`, or leading `-` must stay inside
        // the quoted scalar so the YAML stays valid; entries without a title are
        // dropped entirely (hayagriva requires one).
        let mut hostile = entry("k", "Clever: A \"Quote\" - heavy title", "Doe, John", 2020);
        hostile.metadata.doi = Some("10.1/x\"y".into());
        let titleless = {
            let mut e = entry("no-title", "   ", "X", 2020);
            e.metadata.title = "   ".into();
            e
        };
        let yaml = bibliography_yaml(&[hostile, titleless]);
        assert!(yaml.contains("\"k\":"));
        assert!(!yaml.contains("no-title"));
        // The colons inside the title are inside the quoted scalar, and the
        // embedded quotes are backslash-escaped rather than terminating it.
        assert!(yaml.contains("  title: \"Clever: A \\\"Quote\\\" - heavy title\""));
        assert!(yaml.contains("    doi: \"10.1/x\\\"y\""));
        // Round-trip through a simple YAML-ish check: a `:` inside a title is
        // inside the quoted scalar, so it does not become a new top-level key.
        // Count column-0 quoted keys (the only place an entry can begin).
        let top_level_keys = yaml
            .lines()
            .filter(|line| line.starts_with('"') && line.trim_end().ends_with(':'))
            .count();
        assert_eq!(top_level_keys, 1);
    }
}
