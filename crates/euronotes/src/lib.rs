//! EuroNotes — de notitie-app van EuroOS (Sprint AC-1).
//!
//! Parseert **Markdown** naar het [`eurodoc`]-UDM (dezelfde blokken die EuroSuite
//! Writer rendert), zodat een notitie meteen in de compositor getoond kan worden.
//! Ondersteunt koppen (`#`..`######`), **vet**/*cursief*/`code`, opsommings- en
//! genummerde lijsten met inspringing, citaten (`>`), horizontale regels (`---`)
//! en code-blokken (```). Extraheert inline `#tags` en draagt een
//! **append-only**-vlag voor tamper-evidente (audit-)notities.
//!
//! Pure `no_std`-logica, host-getest.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use eurodoc::model::{Block, Paragraph, Run};

/// Een geparste notitie: titel, inhoud als UDM-blokken, en afgeleide metadata.
#[derive(Debug, Clone)]
pub struct Note {
    pub title: String,
    pub blocks: Vec<Block>,
    pub tags: Vec<String>,
    /// Tamper-evident: eenmaal gezet mag de notitie enkel groeien, nooit wijzigen.
    pub append_only: bool,
}

/// Parse een Markdown-string naar een [`Note`].
pub fn parse(md: &str) -> Note {
    let mut blocks = Vec::new();
    let mut tags = Vec::new();
    let mut title = String::new();

    let lines: Vec<&str> = md.lines().collect();
    let mut i = 0;
    let mut para: Vec<String> = Vec::new();

    // Hulp: spoel een opgebouwde paragraaf (samengevoegde regels) weg.
    macro_rules! flush_para {
        () => {
            if !para.is_empty() {
                let text = para.join(" ");
                collect_tags(&text, &mut tags);
                blocks.push(Block::Paragraph(Paragraph {
                    props: Default::default(),
                    runs: parse_inline(&text),
                }));
                para.clear();
            }
        };
    }

    while i < lines.len() {
        let raw = lines[i];
        let line = raw.trim_end();
        let trimmed = line.trim_start();

        // Code-blok via ``` fences.
        if trimmed.starts_with("```") {
            flush_para!();
            i += 1;
            while i < lines.len() && !lines[i].trim_start().starts_with("```") {
                let mut p = Paragraph::text(lines[i]).styled("Code");
                if let Some(r) = p.runs.first_mut() {
                    r.props.font_family = Some("EuroMono".to_string());
                }
                blocks.push(Block::Paragraph(p));
                i += 1;
            }
            i += 1; // sluitende fence
            continue;
        }

        // Lege regel → paragraaf-einde.
        if trimmed.is_empty() {
            flush_para!();
            i += 1;
            continue;
        }

        // Horizontale regel.
        if is_hr(trimmed) {
            flush_para!();
            blocks.push(Block::HorizontalRule);
            i += 1;
            continue;
        }

        // Kop.
        if let Some((level, rest)) = heading(trimmed) {
            flush_para!();
            collect_tags(rest, &mut tags);
            let style = alloc::format!("Heading{level}");
            let mut p = Paragraph { props: Default::default(), runs: parse_inline(rest) };
            p = p.styled(&style);
            if title.is_empty() && level == 1 {
                title = rest.trim().to_string();
            }
            blocks.push(Block::Paragraph(p));
            i += 1;
            continue;
        }

        // Citaat.
        if let Some(rest) = trimmed.strip_prefix('>') {
            flush_para!();
            let rest = rest.trim_start();
            collect_tags(rest, &mut tags);
            let p = Paragraph { props: Default::default(), runs: parse_inline(rest) }.styled("Quote");
            blocks.push(Block::Paragraph(p));
            i += 1;
            continue;
        }

        // Lijstitem (opsomming of genummerd), niveau via inspringing.
        if let Some((content, _ordered)) = list_item(trimmed) {
            flush_para!();
            let indent = line.len() - trimmed.len();
            let level = (indent / 2).min(8) as u8;
            collect_tags(content, &mut tags);
            let mut p = Paragraph { props: Default::default(), runs: parse_inline(content) };
            p.props.list_level = Some(level);
            blocks.push(Block::Paragraph(p));
            i += 1;
            continue;
        }

        // Anders: gewone paragraaf-regel (accumuleren tot lege regel).
        para.push(trimmed.to_string());
        i += 1;
    }
    flush_para!();

    if title.is_empty() {
        // Val terug op de eerste niet-lege tekst.
        title = blocks
            .iter()
            .find_map(|b| match b {
                Block::Paragraph(p) => {
                    let t = p.plain_text();
                    if t.trim().is_empty() {
                        None
                    } else {
                        Some(t.trim().to_string())
                    }
                }
                _ => None,
            })
            .unwrap_or_else(|| "Naamloze notitie".to_string());
    }

    // Dedup tags, volgorde behouden.
    let mut seen = Vec::new();
    tags.retain(|t| {
        if seen.contains(t) {
            false
        } else {
            seen.push(t.clone());
            true
        }
    });

    Note { title, blocks, tags, append_only: false }
}

fn is_hr(s: &str) -> bool {
    let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    s.len() >= 3
        && (s.chars().all(|c| c == '-') || s.chars().all(|c| c == '*') || s.chars().all(|c| c == '_'))
}

fn heading(s: &str) -> Option<(u8, &str)> {
    let hashes = s.chars().take_while(|&c| c == '#').count();
    if hashes >= 1 && hashes <= 6 {
        let rest = &s[hashes..];
        if rest.starts_with(' ') {
            return Some((hashes as u8, rest.trim_start()));
        }
    }
    None
}

fn list_item(s: &str) -> Option<(&str, bool)> {
    for marker in ["- ", "* ", "+ "] {
        if let Some(rest) = s.strip_prefix(marker) {
            return Some((rest, false));
        }
    }
    // Genummerd: "N. " of "N) ".
    let digits = s.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits >= 1 && digits <= 9 {
        let after = &s[digits..];
        if let Some(rest) = after.strip_prefix(". ").or_else(|| after.strip_prefix(") ")) {
            return Some((rest, true));
        }
    }
    None
}

/// Verzamel `#tag`-tokens uit tekst (letters/cijfers/`-`/`_`, min. 1 teken).
fn collect_tags(text: &str, out: &mut Vec<String>) {
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '#' && (i == 0 || chars[i - 1].is_whitespace()) {
            let start = i + 1;
            let mut j = start;
            while j < chars.len() && (chars[j].is_ascii_alphanumeric() || chars[j] == '-' || chars[j] == '_') {
                j += 1;
            }
            if j > start {
                let tag: String = chars[start..j].iter().collect();
                // Niet puur numeriek (anders is het geen tag maar bv. "#1").
                if !tag.chars().all(|c| c.is_ascii_digit()) {
                    out.push(tag);
                }
            }
            i = j;
        } else {
            i += 1;
        }
    }
}

/// Parse inline-opmaak (`**vet**`, `*cursief*`, `` `code` ``) naar runs.
fn parse_inline(text: &str) -> Vec<Run> {
    let chars: Vec<char> = text.chars().collect();
    let mut runs = Vec::new();
    let mut buf = String::new();
    let mut bold = false;
    let mut italic = false;
    let mut i = 0;

    macro_rules! flush_buf {
        () => {
            if !buf.is_empty() {
                let mut r = Run::new(&buf);
                r.props.bold = bold;
                r.props.italic = italic;
                runs.push(r);
                buf.clear();
            }
        };
    }

    while i < chars.len() {
        // Inline code: `...`
        if chars[i] == '`' {
            flush_buf!();
            i += 1;
            let start = i;
            while i < chars.len() && chars[i] != '`' {
                i += 1;
            }
            let code: String = chars[start..i].iter().collect();
            let mut r = Run::new(&code);
            r.props.font_family = Some("EuroMono".to_string());
            runs.push(r);
            if i < chars.len() {
                i += 1; // sluitende backtick
            }
            continue;
        }
        // Vet: ** of __
        if (chars[i] == '*' || chars[i] == '_') && i + 1 < chars.len() && chars[i + 1] == chars[i] {
            flush_buf!();
            bold = !bold;
            i += 2;
            continue;
        }
        // Cursief: enkele * of _
        if chars[i] == '*' || chars[i] == '_' {
            flush_buf!();
            italic = !italic;
            i += 1;
            continue;
        }
        buf.push(chars[i]);
        i += 1;
    }
    flush_buf!();
    if runs.is_empty() {
        runs.push(Run::new(""));
    }
    runs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn para_text(b: &Block) -> String {
        match b {
            Block::Paragraph(p) => p.plain_text(),
            _ => String::new(),
        }
    }
    fn style_of(b: &Block) -> Option<String> {
        match b {
            Block::Paragraph(p) => p.props.style_id.clone(),
            _ => None,
        }
    }

    #[test]
    fn headings_and_title() {
        let note = parse("# EuroOS notities\n\nGewone tekst.");
        assert_eq!(note.title, "EuroOS notities");
        assert_eq!(style_of(&note.blocks[0]).as_deref(), Some("Heading1"));
        assert_eq!(para_text(&note.blocks[0]), "EuroOS notities");
        assert_eq!(para_text(&note.blocks[1]), "Gewone tekst.");
    }

    #[test]
    fn heading_levels() {
        let note = parse("## Twee\n### Drie");
        assert_eq!(style_of(&note.blocks[0]).as_deref(), Some("Heading2"));
        assert_eq!(style_of(&note.blocks[1]).as_deref(), Some("Heading3"));
    }

    #[test]
    fn inline_bold_italic_code() {
        let note = parse("Dit is **vet**, *cursief* en `code`.");
        if let Block::Paragraph(p) = &note.blocks[0] {
            let bold = p.runs.iter().find(|r| r.text == "vet").unwrap();
            assert!(bold.props.bold);
            let ital = p.runs.iter().find(|r| r.text == "cursief").unwrap();
            assert!(ital.props.italic);
            let code = p.runs.iter().find(|r| r.text == "code").unwrap();
            assert_eq!(code.props.font_family.as_deref(), Some("EuroMono"));
        } else {
            panic!();
        }
    }

    #[test]
    fn lists_with_levels() {
        let note = parse("- een\n- twee\n  - genest\n1. eerste");
        let levels: Vec<Option<u8>> = note
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::Paragraph(p) => p.props.list_level,
                _ => None,
            })
            .map(Some)
            .collect();
        assert_eq!(levels, alloc::vec![Some(0), Some(0), Some(1), Some(0)]);
    }

    #[test]
    fn horizontal_rule_and_quote() {
        let note = parse("> citaat\n\n---\n\ntekst");
        assert!(note.blocks.iter().any(|b| matches!(b, Block::HorizontalRule)));
        assert!(note
            .blocks
            .iter()
            .any(|b| style_of(b).as_deref() == Some("Quote")));
    }

    #[test]
    fn code_block_fenced() {
        let note = parse("```\nlet x = 1;\nlet y = 2;\n```");
        let code_lines: Vec<String> = note
            .blocks
            .iter()
            .filter(|b| style_of(b).as_deref() == Some("Code"))
            .map(para_text)
            .collect();
        assert_eq!(code_lines, alloc::vec!["let x = 1;", "let y = 2;"]);
    }

    #[test]
    fn tag_extraction() {
        let note = parse("Plan voor #euros en #q3-2026. Niet #123.\n\n#project nog een.");
        assert!(note.tags.contains(&"euros".to_string()));
        assert!(note.tags.contains(&"q3-2026".to_string()));
        assert!(note.tags.contains(&"project".to_string()));
        assert!(!note.tags.contains(&"123".to_string())); // puur numeriek genegeerd
    }

    #[test]
    fn paragraph_lines_joined() {
        let note = parse("regel een\nregel twee\n\nnieuwe paragraaf");
        // Eerste twee regels → één paragraaf.
        assert_eq!(para_text(&note.blocks[0]), "regel een regel twee");
        assert_eq!(para_text(&note.blocks[1]), "nieuwe paragraaf");
    }
}
