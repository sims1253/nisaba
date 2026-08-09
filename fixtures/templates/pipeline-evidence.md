# Template-pipeline evidence record

Use this form when validating a real DOCX-to-Typst conversion. Keep proprietary source files
outside the repository and record only approved hashes, versions, measurements, and conclusions.

## Provenance

- Source description: <<document and version>>
- Source SHA-256: <<hash>>
- Conversion commit: <<commit>>
- Tool versions: <<versions>>
- Operator/date: <<name / ISO date>>

## Structural checks

- [ ] Headings and outline levels preserved
- [ ] Required placeholders represented
- [ ] Tables, numbering, headers, footers, and page geometry reviewed
- [ ] Images and hyperlinks resolved
- [ ] Reference metadata round-trips without loss on required fields

## Visual checks

- Reference PDF: <<path or object-store identifier>>
- Candidate PDF: <<path or object-store identifier>>
- Render DPI and thresholds: <<values>>
- Maximum/mean page difference: <<values>>
- Pages inspected manually: <<range>>

## Result

- Status: <<pass / fail / follow-up>>
- Known differences: <<notes>>
- Follow-up owner: <<owner>>
