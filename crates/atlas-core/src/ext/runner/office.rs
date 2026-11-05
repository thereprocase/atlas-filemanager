#![allow(clippy::disallowed_methods)]
// reason: preview functions run on blocking workers and must touch files.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use atlas_model::preview_cache::{PreviewResult, SlidePreview};

pub(crate) fn builtin_document_preview(
    path: &Path,
    tools: &HashMap<String, PathBuf>,
) -> PreviewResult {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "csv" => delimited_preview(path, b','),
        "tsv" => delimited_preview(path, b'\t'),
        "xls" | "xlsm" | "xlsx" | "ods" => spreadsheet_preview(path),
        "docx" => crate::fs::preview::document::preview_docx(path),
        "pptx" => pptx_preview(path),
        "pdf" => {
            let Some(tool) = tools.get("pdftoppm") else {
                return PreviewResult::Error("Preview unavailable: pdftoppm not found".into());
            };
            crate::fs::preview::preview_pdf_with_pdftoppm(path, tool)
        }
        "psd" | "psb" => {
            if let Some(tool) = tools.get("magick") {
                super::img::run_magick_preview(path, tool)
            } else {
                PreviewResult::Error("Preview unavailable: ImageMagick not found".into())
            }
        }
        _ => PreviewResult::Error(format!("No previewer for .{ext}")),
    }
}

/// Cap on bytes read for a delimited preview; enough for 200 typical rows.
const DELIMITED_PREVIEW_BYTES: u64 = 2 * 1024 * 1024;

fn delimited_preview(path: &Path, default_delimiter: u8) -> PreviewResult {
    use std::io::Read as _;
    let mut bytes = Vec::new();
    let read = std::fs::File::open(path).and_then(|file| {
        file.take(DELIMITED_PREVIEW_BYTES + 1).read_to_end(&mut bytes)
    });
    if let Err(e) = read {
        return PreviewResult::Error(e.to_string());
    }
    let clipped = bytes.len() as u64 > DELIMITED_PREVIEW_BYTES;
    bytes.truncate(DELIMITED_PREVIEW_BYTES as usize);
    delimited_bytes(&bytes, default_delimiter, clipped)
}

pub(crate) fn delimited_bytes(bytes: &[u8], default_delimiter: u8, clipped: bool) -> PreviewResult {
    let text = decode_delimited_text(bytes);
    let delimiter = sniff_delimiter(&text, default_delimiter);
    let (mut rows, records_truncated) = delimited_records(&text, delimiter);
    let truncated = clipped || records_truncated;
    let headers = if rows.is_empty() {
        Vec::new()
    } else {
        rows.remove(0)
    };
    PreviewResult::Table {
        headers,
        rows,
        truncated,
    }
}

/// UTF-8 (BOM stripped) or, failing that, Windows-1252 — what Excel writes on
/// Windows for "CSV (delimitado por comas)".
fn decode_delimited_text(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xff, 0xfe]) || bytes.starts_with(&[0xfe, 0xff]) {
        let little = bytes[0] == 0xff;
        let units: Vec<_> = bytes[2..].chunks_exact(2).map(|pair| {
            if little { u16::from_le_bytes([pair[0], pair[1]]) }
            else { u16::from_be_bytes([pair[0], pair[1]]) }
        }).collect();
        return String::from_utf16_lossy(&units);
    }
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_owned(),
        Err(e) if e.error_len().is_none() && e.valid_up_to() + 4 > bytes.len() => {
            // Only the cut-off tail is invalid (bounded read split a char).
            String::from_utf8_lossy(&bytes[..e.valid_up_to()]).into_owned()
        }
        Err(_) => bytes.iter().map(|&b| windows_1252(b)).collect(),
    }
}

fn windows_1252(byte: u8) -> char {
    const HIGH: [char; 32] = [
        '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8D}', 'Ž',
        '\u{8F}', '\u{90}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\u{9D}',
        'ž', 'Ÿ',
    ];
    match byte {
        0x80..=0x9F => HIGH[(byte - 0x80) as usize],
        _ => byte as char,
    }
}

/// Locale-exported CSVs often use `;` (Spanish/European Excel) or tabs. Pick
/// the candidate that splits the first lines most consistently.
fn sniff_delimiter(text: &str, default: u8) -> u8 {
    let sample: Vec<&str> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .take(20)
        .collect();
    if sample.is_empty() {
        return default;
    }
    let mut best = (default, 0usize);
    for candidate in [default, b',', b';', b'\t', b'|'] {
        let counts: Vec<usize> = sample
            .iter()
            .map(|line| parse_delimited_line(line, candidate).len())
            .collect();
        let first = counts[0];
        if first < 2 {
            continue;
        }
        let consistent = counts.iter().filter(|&&count| count == first).count();
        let score = consistent * 1000 + first;
        if score > best.1 {
            best = (candidate, score);
        }
    }
    best.0
}

fn parse_delimited_line(line: &str, delimiter: u8) -> Vec<String> {
    let delimiter = delimiter as char;
    let mut cells = Vec::new();
    let mut cell = String::new();
    let mut chars = line.chars().peekable();
    let mut quoted = false;
    while let Some(ch) = chars.next() {
        if quoted {
            if ch == '"' {
                if chars.peek() == Some(&'"') {
                    cell.push('"');
                    let _ = chars.next();
                } else {
                    quoted = false;
                }
            } else {
                cell.push(ch);
            }
        } else if ch == '"' && cell.is_empty() {
            quoted = true;
        } else if ch == delimiter {
            cells.push(std::mem::take(&mut cell));
        } else {
            cell.push(ch);
        }
    }
    cells.push(cell);
    cells
}

fn delimited_records(text: &str, delimiter: u8) -> (Vec<Vec<String>>, bool) {
    let mut rows = Vec::new();
    let mut truncated = false;
    let mut quoted = false;
    let mut field_start = true;
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((index, ch)) = chars.next() {
        if ch == '"' {
            if quoted && chars.peek().is_some_and(|(_, next)| *next == '"') { chars.next(); }
            else if quoted { quoted = false; }
            else if field_start { quoted = true; }
            field_start = false;
        } else if !quoted && ch == '\n' {
            let record = text[start..index].trim_end_matches('\r');
            start = index + 1;
            field_start = true;
            if record.trim().is_empty() { continue; }
            if rows.len() == 201 { return (rows, true); }
            let mut cells = parse_delimited_line(record, delimiter);
            truncated |= cells.len() > 50;
            cells.truncate(50);
            rows.push(cells);
        } else if !quoted { field_start = ch == char::from(delimiter); }
    }
    let record = text[start..].trim_end_matches('\r');
    if !record.trim().is_empty() {
        if rows.len() == 201 { return (rows, true); }
        let mut cells = parse_delimited_line(record, delimiter);
        truncated |= cells.len() > 50;
        cells.truncate(50);
        rows.push(cells);
    }
    (rows, truncated || quoted)
}

fn spreadsheet_preview(path: &Path) -> PreviewResult {
    use calamine::{open_workbook_auto, Reader};
    let mut workbook = match open_workbook_auto(path) {
        Ok(w) => w,
        Err(e) => return PreviewResult::Error(e.to_string()),
    };
    let Some(sheet) = workbook.sheet_names().first().cloned() else {
        return PreviewResult::Table {
            headers: Vec::new(),
            rows: Vec::new(),
            truncated: false,
        };
    };
    let range = match workbook.worksheet_range(&sheet) {
        Ok(r) => r,
        Err(e) => return PreviewResult::Error(e.to_string()),
    };
    let mut rows: Vec<Vec<String>> = range
        .rows()
        .take(201)
        .map(|row| row.iter().take(50).map(cell_to_string).collect())
        .collect();
    let truncated = range.height() > 201 || range.width() > 50;
    let headers = if rows.is_empty() {
        Vec::new()
    } else {
        rows.remove(0)
    };
    PreviewResult::Table {
        headers,
        rows,
        truncated,
    }
}

fn cell_to_string(cell: &calamine::Data) -> String {
    match cell {
        calamine::Data::Empty => String::new(),
        calamine::Data::String(s) => s.clone(),
        calamine::Data::Float(n) => n.to_string(),
        calamine::Data::Int(n) => n.to_string(),
        calamine::Data::Bool(v) => v.to_string(),
        calamine::Data::DateTime(v) => v.to_string(),
        calamine::Data::DateTimeIso(s) | calamine::Data::DurationIso(s) => s.clone(),
        calamine::Data::Error(e) => format!("{e:?}"),
    }
}

fn pptx_preview(path: &Path) -> PreviewResult {
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) => return PreviewResult::Error(e.to_string()),
    };
    let mut zip = match zip::ZipArchive::new(file) {
        Ok(z) => z,
        Err(e) => return PreviewResult::Error(e.to_string()),
    };
    let mut names: Vec<String> = (0..zip.len())
        .filter_map(|i| zip.by_index(i).ok().map(|f| f.name().to_string()))
        .filter(|name| name.starts_with("ppt/slides/slide") && name.ends_with(".xml"))
        .collect();
    names.sort();
    let mut slides = Vec::new();
    for name in names.into_iter().take(50) {
        let Ok(entry) = zip.by_name(&name) else {
            continue;
        };
        const MAX_SLIDE_XML: u64 = 8 * 1024 * 1024;
        if entry.size() > MAX_SLIDE_XML { return PreviewResult::Error("Slide XML exceeds 8 MiB".into()); }
        let mut xml = String::new();
        use std::io::Read as _;
        if let Err(error) = entry.take(MAX_SLIDE_XML + 1).read_to_string(&mut xml) { return PreviewResult::Error(error.to_string()); }
        if xml.len() as u64 > MAX_SLIDE_XML { return PreviewResult::Error("Slide XML exceeds 8 MiB".into()); }
        {
            let blocks = xml_text_blocks(&xml);
            let title = blocks.first().cloned().unwrap_or_default();
            slides.push(SlidePreview {
                title,
                body: blocks.into_iter().skip(1).take(12).collect(),
            });
        }
    }
    PreviewResult::Presentation {
        truncated: slides.len() >= 50,
        slides,
    }
}

pub(crate) fn xml_text_blocks(xml: &str) -> Vec<String> {
    let mut out = String::new();
    let mut in_tag = false;
    for ch in xml.chars() {
        match ch {
            '<' => {
                in_tag = true;
                out.push('\n');
            }
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_line_parser_handles_quotes() {
        assert_eq!(
            parse_delimited_line(r#"a,"b,c","d""e""#, b','),
            vec!["a", "b,c", "d\"e"]
        );
    }

    #[test]
    fn xml_text_extracts_basic_ooxml_text() {
        assert_eq!(
            xml_text_blocks("<w:p><w:t>Hello</w:t></w:p><w:p><w:t>World</w:t></w:p>"),
            vec!["Hello", "World"]
        );
    }

    #[test]
    fn word_extension_uses_native_docx_reader() {
        use std::io::Write as _;
        let file = tempfile::Builder::new().suffix(".docx").tempfile().unwrap();
        let mut writer = zip::ZipWriter::new(std::fs::File::create(file.path()).unwrap());
        writer.start_file("word/document.xml", zip::write::SimpleFileOptions::default()).unwrap();
        writer.write_all(br#"<document><p><r><t>Hello </t></r><r><t>world &#233;</t></r></p></document>"#).unwrap();
        writer.finish().unwrap();
        assert!(matches!(builtin_document_preview(file.path(), &HashMap::new()), PreviewResult::Document { blocks, .. } if blocks == ["Hello world é"]));
    }
}

#[cfg(test)]
mod delimited_tests {
    use super::*;

    #[test]
    fn sniffs_semicolons_and_strips_bom() {
        let text = decode_delimited_text("\u{FEFF}nombre;precio\nlápiz;0,85\n".as_bytes());
        assert_eq!(sniff_delimiter(&text, b','), b';');
        assert_eq!(parse_delimited_line(text.lines().next().unwrap(), b';'), ["nombre", "precio"]);
    }

    #[test]
    fn keeps_commas_when_consistent() {
        let text = "a,b,c\n1,\"x; y\",3\n4,5,6\n";
        assert_eq!(sniff_delimiter(text, b','), b',');
    }

    #[test]
    fn decodes_windows_1252() {
        assert_eq!(decode_delimited_text(b"caf\xE9;\x80"), "café;€");
    }

    #[test]
    fn multiline_quotes_and_unicode_stay_in_one_record() {
        let (rows, truncated) = delimited_records("nombre;nota\nniño;\"primera línea\n日本語 y \"\"comillas\"\"\"\n", b';');
        assert!(!truncated);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1], ["niño", "primera línea\n日本語 y \"comillas\""]);
        let mut utf16 = vec![0xff, 0xfe];
        utf16.extend("nombre;niño".encode_utf16().flat_map(u16::to_le_bytes));
        assert_eq!(decode_delimited_text(&utf16), "nombre;niño");
    }

    #[test]
    fn delimited_preview_limits_rows_and_columns() {
        let (rows, truncated) = delimited_records(&"a,b\n".repeat(300), b',');
        assert!(truncated);
        assert_eq!(rows.len(), 201);
        let (rows, truncated) = delimited_records(&"x,".repeat(80), b',');
        assert!(truncated);
        assert_eq!(rows[0].len(), 50);
    }
}

#[cfg(test)]
mod macro_workbook_tests {
    use super::*;
    #[test]
    fn xlsm_preview_reads_cells_and_ignores_macro_payload() {
        use std::io::Write;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("parts.xlsm");
        let mut archive = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        let files = [
            ("[Content_Types].xml", r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.ms-excel.sheet.macroEnabled.main+xml"/></Types>"#),
            ("xl/workbook.xml", r#"<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="Parts" sheetId="1" r:id="rId1"/></sheets></workbook>"#),
            ("xl/_rels/workbook.xml.rels", r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/></Relationships>"#),
            ("xl/worksheets/sheet1.xml", r#"<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetData><row r="1"><c r="A1" t="inlineStr"><is><t>Part</t></is></c></row><row r="2"><c r="A2" t="inlineStr"><is><t>Bracket</t></is></c></row></sheetData></worksheet>"#),
            ("xl/vbaProject.bin", "inert audit fixture; this payload must not be parsed as workbook cells"),
        ];
        for (name, text) in files { archive.start_file(name, zip::write::SimpleFileOptions::default()).unwrap(); archive.write_all(text.as_bytes()).unwrap(); }
        archive.finish().unwrap();
        let result = builtin_document_preview(&path, &HashMap::new());
        match result {
            PreviewResult::Table { headers, rows, .. } => { assert_eq!(headers, ["Part"]); assert_eq!(rows, [vec!["Bracket".to_string()]]); }
            other => panic!("expected workbook cells, got {other:?}"),
        }
    }
}
