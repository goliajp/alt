//! OOXML semantic extractors. Pulls the human-readable content out of
//! `word/document.xml` (one entry per paragraph) and out of an `.xlsx`
//! workbook (one entry per non-empty cell, rendered as
//! `Sheet!Ref → value`), so the diff API can hand the SPA a
//! reviewer-friendly stream instead of a wall of namespaced XML.
//!
//! Deliberately regex-free: a small linear scan over the XML bytes
//! covers `<w:t>` / `<w:p>` / `<si>` / `<c r="…" t="…">` shapes well
//! enough for the diff demo. A malformed file produces a partial /
//! empty result rather than panicking — the caller falls back to the
//! existing part-level summary.

use std::collections::BTreeMap;

/// Decode the handful of XML entities the OOXML serializer emits.
/// Anything else is passed through verbatim.
fn decode_entities(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// Extract one entry per `<w:p>` paragraph from a Word document.xml,
/// joining every `<w:t>...</w:t>` run inside it. Empty paragraphs are
/// preserved as empty strings so paragraph order is stable across
/// before/after — the diff renderer treats blank paragraphs as visible
/// line breaks.
pub fn docx_paragraphs(xml: &[u8]) -> Vec<String> {
    let text = match std::str::from_utf8(xml) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let mut out: Vec<String> = Vec::new();
    let bytes = text.as_bytes();
    let mut at: usize = 0;
    while at < bytes.len() {
        let Some(p_off) = find_at(text, "<w:p", at) else {
            break;
        };
        // body of this paragraph runs until the next <w:p ...> or end
        let body_start = match text[p_off..].find('>') {
            Some(rel) => p_off + rel + 1,
            None => break,
        };
        let body_end = find_at(text, "<w:p", body_start).unwrap_or(text.len());

        let body = &text[body_start..body_end];
        let mut paragraph = String::new();
        let mut bat = 0;
        while bat < body.len() {
            // Find the next <w:t opening.
            let Some(t_off) = find_at(body, "<w:t", bat) else {
                break;
            };
            // Skip to after the opening tag's `>`.
            let inner_start = match body[t_off..].find('>') {
                Some(rel) => t_off + rel + 1,
                None => break,
            };
            // Find </w:t>.
            let close = match body[inner_start..].find("</w:t>") {
                Some(rel) => inner_start + rel,
                None => break,
            };
            paragraph.push_str(&body[inner_start..close]);
            bat = close + "</w:t>".len();
        }
        out.push(decode_entities(&paragraph));
        at = body_end;
    }
    out
}

/// Extract `(sheet_name, cell_ref, rendered_value)` triples from every
/// non-empty cell in an .xlsx workbook. The caller hands in the loaded
/// `Sheet*.xml` bodies plus the `sharedStrings.xml` body (or `None`).
///
/// The output is suitable for joining one entry per line and running a
/// line diff over — the reviewer sees a stream of
/// `Plans!D4: 99 → 149` style cell updates without having to think
/// about how OOXML structures sheets.
pub fn xlsx_cells(
    sheets: &BTreeMap<String, Vec<u8>>,
    shared_strings_xml: Option<&[u8]>,
    workbook_xml: Option<&[u8]>,
) -> Vec<XlsxCell> {
    let shared = shared_strings_xml
        .map(xlsx_shared_strings)
        .unwrap_or_default();
    // Sheet display names come from workbook.xml's <sheet name="..." r:id=".."/>
    // referencing `_rels/workbook.xml.rels` to map r:id to the actual file
    // path. To avoid that two-hop indirection here we just use the file's
    // basename ("sheet1" → "Sheet1") which is good enough for review.
    let sheet_names = workbook_xml.map(xlsx_sheet_names).unwrap_or_default();
    let mut out = Vec::new();
    for (path, body) in sheets {
        // `xl/worksheets/sheet1.xml` → "sheet1"
        let base = std::path::Path::new(path)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(path);
        let label = sheet_names
            .get(base)
            .cloned()
            .unwrap_or_else(|| capitalise(base));
        let cells = xlsx_sheet_cells(body, &shared, &label);
        out.extend(cells);
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XlsxCell {
    pub sheet: String,
    pub cell_ref: String,
    pub value: String,
}

impl XlsxCell {
    /// Render the line a paragraph-style diff renders one per row.
    pub fn render(&self) -> String {
        format!("{}!{}: {}", self.sheet, self.cell_ref, self.value)
    }
}

fn xlsx_shared_strings(xml: &[u8]) -> Vec<String> {
    let text = match std::str::from_utf8(xml) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let mut out: Vec<String> = Vec::new();
    let mut at = 0;
    while let Some(si_off) = find_at(text, "<si", at) {
        let body_start = match text[si_off..].find('>') {
            Some(rel) => si_off + rel + 1,
            None => break,
        };
        let body_end = find_at(text, "</si>", body_start).unwrap_or(text.len());
        let body = &text[body_start..body_end];
        // join every <t>...</t> in the si entry
        let mut s = String::new();
        let mut bat = 0;
        while let Some(t_off) = find_at(body, "<t", bat) {
            let inner_start = match body[t_off..].find('>') {
                Some(rel) => t_off + rel + 1,
                None => break,
            };
            let close = match body[inner_start..].find("</t>") {
                Some(rel) => inner_start + rel,
                None => break,
            };
            s.push_str(&body[inner_start..close]);
            bat = close + "</t>".len();
        }
        out.push(decode_entities(&s));
        at = body_end + "</si>".len();
    }
    out
}

fn xlsx_sheet_names(xml: &[u8]) -> BTreeMap<String, String> {
    // <sheet name="Plans" sheetId="1" r:id="rId1"/>
    // We can't follow r:id without the rels — best-effort by sheetId.
    let text = match std::str::from_utf8(xml) {
        Ok(s) => s,
        Err(_) => return BTreeMap::new(),
    };
    let mut out = BTreeMap::new();
    let mut at = 0;
    while let Some(off) = find_at(text, "<sheet ", at) {
        let close = match text[off..].find("/>") {
            Some(rel) => off + rel,
            None => break,
        };
        let tag = &text[off..close];
        let name = attr_value(tag, "name").unwrap_or_default();
        let sheet_id = attr_value(tag, "sheetId").unwrap_or_default();
        if !sheet_id.is_empty() {
            out.insert(format!("sheet{sheet_id}"), name);
        }
        at = close + 2;
    }
    out
}

fn xlsx_sheet_cells(xml: &[u8], shared: &[String], sheet: &str) -> Vec<XlsxCell> {
    let text = match std::str::from_utf8(xml) {
        Ok(s) => s,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    let mut at = 0;
    while let Some(c_off) = find_at(text, "<c ", at) {
        let head_end = match text[c_off..].find('>') {
            Some(rel) => c_off + rel,
            None => break,
        };
        let head = &text[c_off..head_end];
        let self_closing = head.ends_with('/');
        let cell_ref = attr_value(head, "r").unwrap_or_default();
        let cell_type = attr_value(head, "t").unwrap_or_default();
        let body_start = head_end + 1;
        let body_end = if self_closing {
            body_start
        } else {
            find_at(text, "</c>", body_start).unwrap_or(text.len())
        };
        let body = &text[body_start..body_end];

        let value = cell_value(body, &cell_type, shared);
        if !value.is_empty() && !cell_ref.is_empty() {
            out.push(XlsxCell {
                sheet: sheet.to_string(),
                cell_ref,
                value,
            });
        }
        at = body_end + if self_closing { 1 } else { "</c>".len() };
    }
    out
}

fn cell_value(body: &str, kind: &str, shared: &[String]) -> String {
    // shared-string cell: <v>idx</v> in shared[idx]
    if kind == "s" {
        if let Some(v) = inner(body, "<v>", "</v>")
            && let Ok(idx) = v.parse::<usize>()
            && idx < shared.len()
        {
            return shared[idx].clone();
        }
        return String::new();
    }
    // inline string: <is><t>...</t></is>
    if (kind == "inlineStr" || kind == "str")
        && let Some(t) = inner(body, "<t>", "</t>").or_else(|| inner(body, "<is><t>", "</t></is>"))
    {
        return decode_entities(&t);
    }
    // plain number / boolean: <v>...</v>
    if let Some(v) = inner(body, "<v>", "</v>") {
        return v;
    }
    String::new()
}

// ── tiny scanning helpers ──────────────────────────────────────────

fn find_at(haystack: &str, needle: &str, start: usize) -> Option<usize> {
    haystack[start..].find(needle).map(|p| p + start)
}

fn attr_value(tag: &str, name: &str) -> Option<String> {
    let needle = format!("{name}=\"");
    let start = tag.find(&needle)? + needle.len();
    let end = tag[start..].find('"')? + start;
    Some(decode_entities(&tag[start..end]))
}

fn inner(body: &str, open: &str, close: &str) -> Option<String> {
    let start = body.find(open)? + open.len();
    let end = body[start..].find(close)? + start;
    Some(body[start..end].to_string())
}

fn capitalise(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(first) => first.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docx_paragraphs_extracts_each_w_p_block_in_order() {
        let xml = b"<w:document><w:body>\
            <w:p><w:r><w:t>Hello</w:t></w:r></w:p>\
            <w:p><w:r><w:t xml:space=\"preserve\">Second </w:t>\
                  <w:t>line</w:t></w:r></w:p>\
            <w:p/></w:body></w:document>";
        let paragraphs = docx_paragraphs(xml);
        assert_eq!(
            paragraphs,
            vec![
                "Hello".to_string(),
                "Second line".to_string(),
                "".to_string()
            ]
        );
    }

    #[test]
    fn docx_paragraphs_decodes_entities() {
        let xml = b"<w:p><w:r><w:t>x &amp; y &lt; z</w:t></w:r></w:p>";
        let p = docx_paragraphs(xml);
        assert_eq!(p, vec!["x & y < z".to_string()]);
    }

    #[test]
    fn xlsx_shared_strings_collects_each_si() {
        let xml = b"<sst><si><t>Personal</t></si>\
                    <si><t>Team</t></si><si><t>Org</t></si></sst>";
        let strings = xlsx_shared_strings(xml);
        assert_eq!(strings, vec!["Personal", "Team", "Org"]);
    }

    #[test]
    fn xlsx_sheet_cells_handles_shared_inline_and_numeric() {
        let shared = vec!["Personal".to_string(), "Team".to_string()];
        let sheet = b"<worksheet><sheetData>\
            <row r=\"1\"><c r=\"A1\" t=\"s\"><v>0</v></c></row>\
            <row r=\"2\"><c r=\"B2\"><v>123</v></c></row>\
            <row r=\"3\"><c r=\"C3\" t=\"inlineStr\"><is><t>raw</t></is></c></row>\
        </sheetData></worksheet>";
        let cells = xlsx_sheet_cells(sheet, &shared, "Plans");
        assert_eq!(
            cells,
            vec![
                XlsxCell {
                    sheet: "Plans".into(),
                    cell_ref: "A1".into(),
                    value: "Personal".into()
                },
                XlsxCell {
                    sheet: "Plans".into(),
                    cell_ref: "B2".into(),
                    value: "123".into()
                },
                XlsxCell {
                    sheet: "Plans".into(),
                    cell_ref: "C3".into(),
                    value: "raw".into()
                },
            ]
        );
    }
}
