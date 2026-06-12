//! EuroWeb HTML5-tokenizer — de eerste fase van de engine.
//!
//! Een toestandsmachine die een bytestroom HTML omzet in een reeks [`Token`]s
//! (start-/eindtags, tekst, commentaar, DOCTYPE), volgens het WHATWG HTML Living
//! Standard "tokenization"-hoofdstuk. Geïmplementeerd subset dekt het echte web:
//! tags + attributen (alle vier de waarde-vormen), commentaar, DOCTYPE,
//! self-closing, **character references** (named + numeriek), en de
//! **RAWTEXT/RCDATA** content-modellen voor `script/style` resp. `title/textarea`.
//!
//! Pure `no_std`-logica, host-getest tegen HTML5lib-achtige gevallen.

use alloc::string::String;
use alloc::vec::Vec;

use crate::dom::Attr;
use crate::entities::decode_entity;

/// Een tokenizer-token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Token {
    Doctype { name: String, force_quirks: bool },
    StartTag { name: String, attrs: Vec<Attr>, self_closing: bool },
    EndTag { name: String },
    Comment(String),
    /// Eén tekst-character. De parser voegt opeenvolgende characters samen.
    Character(char),
    Eof,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Data,
    TagOpen,
    EndTagOpen,
    TagName,
    BeforeAttrName,
    AttrName,
    AfterAttrName,
    BeforeAttrValue,
    AttrValueDouble,
    AttrValueSingle,
    AttrValueUnquoted,
    AfterAttrValueQuoted,
    SelfClosingStart,
    BogusComment,
    MarkupDeclarationOpen,
    CommentStart,
    Comment,
    CommentEndDash,
    CommentEnd,
    Doctype,
    BeforeDoctypeName,
    DoctypeName,
    AfterDoctypeName,
    BogusDoctype,
    /// RAWTEXT (script, style) of RCDATA (title, textarea). `rcdata` bepaalt of
    /// character references hier nog ontleed worden.
    RawText { rcdata: bool },
}

/// De content-modus waarin de tokenizer na een starttag verdergaat.
fn content_model(tag: &str) -> Option<bool> {
    match tag {
        "script" | "style" | "xmp" | "iframe" | "noembed" | "noframes" => Some(false), // RAWTEXT
        "title" | "textarea" => Some(true),                                            // RCDATA
        _ => None,
    }
}

struct TagBuilder {
    name: String,
    attrs: Vec<Attr>,
    cur_name: String,
    cur_value: String,
    has_cur_attr: bool,
    self_closing: bool,
    is_end: bool,
}

impl TagBuilder {
    fn new(is_end: bool) -> Self {
        TagBuilder {
            name: String::new(),
            attrs: Vec::new(),
            cur_name: String::new(),
            cur_value: String::new(),
            has_cur_attr: false,
            self_closing: false,
            is_end,
        }
    }

    fn start_attr(&mut self) {
        self.commit_attr();
        self.has_cur_attr = true;
    }

    fn commit_attr(&mut self) {
        if self.has_cur_attr && !self.cur_name.is_empty() {
            // Dubbele attribuutnamen: de eerste wint (HTML-regel).
            if !self.attrs.iter().any(|a| a.name == self.cur_name) {
                self.attrs.push(Attr {
                    name: core::mem::take(&mut self.cur_name),
                    value: core::mem::take(&mut self.cur_value),
                });
            } else {
                self.cur_name.clear();
                self.cur_value.clear();
            }
        }
        self.cur_name.clear();
        self.cur_value.clear();
        self.has_cur_attr = false;
    }

    fn finish(mut self) -> Token {
        self.commit_attr();
        if self.is_end {
            Token::EndTag { name: self.name }
        } else {
            Token::StartTag { name: self.name, attrs: self.attrs, self_closing: self.self_closing }
        }
    }
}

/// Tokeniseer een volledige HTML-string naar een reeks tokens (eindigend op [`Token::Eof`]).
pub fn tokenize(input: &str) -> Vec<Token> {
    let chars: Vec<char> = input.chars().collect();
    let mut pos = 0usize;
    let mut state = State::Data;
    let mut tokens: Vec<Token> = Vec::new();
    let mut tag: Option<TagBuilder> = None;
    let mut buf = String::new(); // comment / doctype scratch
    let mut last_start: String = String::new();

    // Hulp om een character reference te ontleden vanaf `pos` (vlak ná de '&').
    // `in_attr` past de attribuut-specifieke regel toe. Retourneert de gedecodeerde
    // tekst en de nieuwe positie.
    fn reference(chars: &[char], pos: usize, in_attr: bool) -> (String, usize) {
        decode_entity(chars, pos, in_attr)
    }

    while pos <= chars.len() {
        let eof = pos == chars.len();
        let c = if eof { '\0' } else { chars[pos] };
        match state {
            State::Data => {
                if eof {
                    tokens.push(Token::Eof);
                    break;
                }
                match c {
                    '&' => {
                        let (txt, np) = reference(&chars, pos + 1, false);
                        for ch in txt.chars() {
                            tokens.push(Token::Character(ch));
                        }
                        pos = np;
                        continue;
                    }
                    '<' => state = State::TagOpen,
                    _ => tokens.push(Token::Character(c)),
                }
            }
            State::RawText { rcdata } => {
                // Verzamel tekst tot een passende eindtag `</last_start>`.
                if eof {
                    tokens.push(Token::Eof);
                    break;
                }
                // Detecteer `</name`-prefix (case-insensitive) gevolgd door een
                // tag-afsluiter, anders behandel als tekst.
                if c == '<' && pos + 1 < chars.len() && chars[pos + 1] == '/' {
                    let name: Vec<char> = last_start.chars().collect();
                    let mut k = pos + 2;
                    let mut matched = true;
                    for nc in &name {
                        if k >= chars.len() || chars[k].to_ascii_lowercase() != *nc {
                            matched = false;
                            break;
                        }
                        k += 1;
                    }
                    let terminator = matched
                        && (k >= chars.len()
                            || matches!(chars[k], ' ' | '\t' | '\n' | '\r' | '\u{0C}' | '/' | '>'));
                    if terminator {
                        // Spring naar de eerste letter van de naam (ná `</`); de
                        // EndTagOpen-state herverwerkt die letter.
                        state = State::EndTagOpen;
                        pos += 2;
                        continue;
                    }
                }
                if rcdata && c == '&' {
                    let (txt, np) = reference(&chars, pos + 1, false);
                    for ch in txt.chars() {
                        tokens.push(Token::Character(ch));
                    }
                    pos = np;
                    continue;
                }
                tokens.push(Token::Character(c));
            }
            State::TagOpen => {
                if c == '!' {
                    state = State::MarkupDeclarationOpen;
                } else if c == '/' {
                    state = State::EndTagOpen;
                } else if c.is_ascii_alphabetic() {
                    tag = Some(TagBuilder::new(false));
                    state = State::TagName;
                    continue; // herverwerk c in TagName
                } else if c == '?' {
                    buf.clear();
                    state = State::BogusComment;
                    continue;
                } else {
                    tokens.push(Token::Character('<'));
                    state = State::Data;
                    continue;
                }
            }
            State::EndTagOpen => {
                if c.is_ascii_alphabetic() {
                    tag = Some(TagBuilder::new(true));
                    state = State::TagName;
                    continue;
                } else if c == '>' {
                    state = State::Data;
                } else if eof {
                    tokens.push(Token::Character('<'));
                    tokens.push(Token::Character('/'));
                    tokens.push(Token::Eof);
                    break;
                } else {
                    buf.clear();
                    state = State::BogusComment;
                    continue;
                }
            }
            State::TagName => {
                let t = tag.as_mut().unwrap();
                match c {
                    ' ' | '\t' | '\n' | '\r' | '\u{0C}' => state = State::BeforeAttrName,
                    '/' => state = State::SelfClosingStart,
                    '>' => {
                        let is_end = t.is_end;
                        let name = t.name.clone();
                        let tok = tag.take().unwrap().finish();
                        tokens.push(tok);
                        state = State::Data;
                        if !is_end {
                            if let Some(rc) = content_model(&name) {
                                last_start = name;
                                state = State::RawText { rcdata: rc };
                            }
                        }
                    }
                    _ if eof => {
                        tokens.push(Token::Eof);
                        break;
                    }
                    _ => t.name.push(c.to_ascii_lowercase()),
                }
            }
            State::BeforeAttrName => {
                let t = tag.as_mut().unwrap();
                match c {
                    ' ' | '\t' | '\n' | '\r' | '\u{0C}' => {}
                    '/' => state = State::SelfClosingStart,
                    '>' => {
                        let (is_end, name) = (t.is_end, t.name.clone());
                        tokens.push(tag.take().unwrap().finish());
                        state = State::Data;
                        if !is_end {
                            if let Some(rc) = content_model(&name) {
                                last_start = name;
                                state = State::RawText { rcdata: rc };
                            }
                        }
                    }
                    _ if eof => {
                        tokens.push(Token::Eof);
                        break;
                    }
                    _ => {
                        t.start_attr();
                        t.cur_name.push(c.to_ascii_lowercase());
                        state = State::AttrName;
                    }
                }
            }
            State::AttrName => {
                let t = tag.as_mut().unwrap();
                match c {
                    ' ' | '\t' | '\n' | '\r' | '\u{0C}' => state = State::AfterAttrName,
                    '/' => {
                        t.commit_attr();
                        state = State::SelfClosingStart;
                    }
                    '=' => state = State::BeforeAttrValue,
                    '>' => {
                        let (is_end, name) = (t.is_end, t.name.clone());
                        tokens.push(tag.take().unwrap().finish());
                        state = State::Data;
                        if !is_end {
                            if let Some(rc) = content_model(&name) {
                                last_start = name;
                                state = State::RawText { rcdata: rc };
                            }
                        }
                    }
                    _ if eof => {
                        tokens.push(Token::Eof);
                        break;
                    }
                    _ => t.cur_name.push(c.to_ascii_lowercase()),
                }
            }
            State::AfterAttrName => {
                let t = tag.as_mut().unwrap();
                match c {
                    ' ' | '\t' | '\n' | '\r' | '\u{0C}' => {}
                    '/' => {
                        t.commit_attr();
                        state = State::SelfClosingStart;
                    }
                    '=' => state = State::BeforeAttrValue,
                    '>' => {
                        let (is_end, name) = (t.is_end, t.name.clone());
                        tokens.push(tag.take().unwrap().finish());
                        state = State::Data;
                        if !is_end {
                            if let Some(rc) = content_model(&name) {
                                last_start = name;
                                state = State::RawText { rcdata: rc };
                            }
                        }
                    }
                    _ if eof => {
                        tokens.push(Token::Eof);
                        break;
                    }
                    _ => {
                        t.start_attr();
                        t.cur_name.push(c.to_ascii_lowercase());
                        state = State::AttrName;
                    }
                }
            }
            State::BeforeAttrValue => {
                match c {
                    ' ' | '\t' | '\n' | '\r' | '\u{0C}' => {}
                    '"' => state = State::AttrValueDouble,
                    '\'' => state = State::AttrValueSingle,
                    '>' => {
                        let t = tag.as_mut().unwrap();
                        let (is_end, name) = (t.is_end, t.name.clone());
                        tokens.push(tag.take().unwrap().finish());
                        state = State::Data;
                        if !is_end {
                            if let Some(rc) = content_model(&name) {
                                last_start = name;
                                state = State::RawText { rcdata: rc };
                            }
                        }
                    }
                    _ if eof => {
                        tokens.push(Token::Eof);
                        break;
                    }
                    _ => {
                        state = State::AttrValueUnquoted;
                        continue;
                    }
                }
            }
            State::AttrValueDouble => {
                if eof {
                    tokens.push(Token::Eof);
                    break;
                }
                match c {
                    '"' => state = State::AfterAttrValueQuoted,
                    '&' => {
                        let (txt, np) = reference(&chars, pos + 1, true);
                        tag.as_mut().unwrap().cur_value.push_str(&txt);
                        pos = np;
                        continue;
                    }
                    _ => tag.as_mut().unwrap().cur_value.push(c),
                }
            }
            State::AttrValueSingle => {
                if eof {
                    tokens.push(Token::Eof);
                    break;
                }
                match c {
                    '\'' => state = State::AfterAttrValueQuoted,
                    '&' => {
                        let (txt, np) = reference(&chars, pos + 1, true);
                        tag.as_mut().unwrap().cur_value.push_str(&txt);
                        pos = np;
                        continue;
                    }
                    _ => tag.as_mut().unwrap().cur_value.push(c),
                }
            }
            State::AttrValueUnquoted => {
                let t = tag.as_mut().unwrap();
                match c {
                    ' ' | '\t' | '\n' | '\r' | '\u{0C}' => state = State::BeforeAttrName,
                    '&' => {
                        let (txt, np) = reference(&chars, pos + 1, true);
                        t.cur_value.push_str(&txt);
                        pos = np;
                        continue;
                    }
                    '>' => {
                        let (is_end, name) = (t.is_end, t.name.clone());
                        tokens.push(tag.take().unwrap().finish());
                        state = State::Data;
                        if !is_end {
                            if let Some(rc) = content_model(&name) {
                                last_start = name;
                                state = State::RawText { rcdata: rc };
                            }
                        }
                    }
                    _ if eof => {
                        tokens.push(Token::Eof);
                        break;
                    }
                    _ => t.cur_value.push(c),
                }
            }
            State::AfterAttrValueQuoted => {
                let t = tag.as_mut().unwrap();
                match c {
                    ' ' | '\t' | '\n' | '\r' | '\u{0C}' => state = State::BeforeAttrName,
                    '/' => {
                        t.commit_attr();
                        state = State::SelfClosingStart;
                    }
                    '>' => {
                        let (is_end, name) = (t.is_end, t.name.clone());
                        tokens.push(tag.take().unwrap().finish());
                        state = State::Data;
                        if !is_end {
                            if let Some(rc) = content_model(&name) {
                                last_start = name;
                                state = State::RawText { rcdata: rc };
                            }
                        }
                    }
                    _ if eof => {
                        tokens.push(Token::Eof);
                        break;
                    }
                    _ => {
                        state = State::BeforeAttrName;
                        continue;
                    }
                }
            }
            State::SelfClosingStart => {
                let t = tag.as_mut().unwrap();
                match c {
                    '>' => {
                        t.self_closing = true;
                        let (is_end, name) = (t.is_end, t.name.clone());
                        tokens.push(tag.take().unwrap().finish());
                        state = State::Data;
                        if !is_end {
                            if let Some(rc) = content_model(&name) {
                                last_start = name;
                                state = State::RawText { rcdata: rc };
                            }
                        }
                    }
                    _ if eof => {
                        tokens.push(Token::Eof);
                        break;
                    }
                    _ => {
                        state = State::BeforeAttrName;
                        continue;
                    }
                }
            }
            State::MarkupDeclarationOpen => {
                // `<!--` → comment, `<!DOCTYPE` → doctype, anders bogus comment.
                if chars[pos..].starts_with(&['-', '-']) {
                    pos += 2;
                    buf.clear();
                    state = State::CommentStart;
                    continue;
                } else if chars[pos..].len() >= 7
                    && chars[pos..pos + 7]
                        .iter()
                        .map(|c| c.to_ascii_lowercase())
                        .eq("doctype".chars())
                {
                    pos += 7;
                    state = State::Doctype;
                    continue;
                } else {
                    buf.clear();
                    state = State::BogusComment;
                    continue;
                }
            }
            State::BogusComment => {
                if eof || c == '>' {
                    tokens.push(Token::Comment(core::mem::take(&mut buf)));
                    state = State::Data;
                    if eof {
                        tokens.push(Token::Eof);
                        break;
                    }
                } else {
                    buf.push(c);
                }
            }
            State::CommentStart => {
                match c {
                    '-' => state = State::CommentEndDash,
                    '>' => {
                        tokens.push(Token::Comment(core::mem::take(&mut buf)));
                        state = State::Data;
                    }
                    _ if eof => {
                        tokens.push(Token::Comment(core::mem::take(&mut buf)));
                        tokens.push(Token::Eof);
                        break;
                    }
                    _ => {
                        state = State::Comment;
                        continue;
                    }
                }
            }
            State::Comment => match c {
                '-' => state = State::CommentEndDash,
                _ if eof => {
                    tokens.push(Token::Comment(core::mem::take(&mut buf)));
                    tokens.push(Token::Eof);
                    break;
                }
                _ => buf.push(c),
            },
            State::CommentEndDash => match c {
                '-' => state = State::CommentEnd,
                _ if eof => {
                    buf.push('-');
                    tokens.push(Token::Comment(core::mem::take(&mut buf)));
                    tokens.push(Token::Eof);
                    break;
                }
                _ => {
                    buf.push('-');
                    state = State::Comment;
                    continue;
                }
            },
            State::CommentEnd => match c {
                '>' => {
                    tokens.push(Token::Comment(core::mem::take(&mut buf)));
                    state = State::Data;
                }
                '-' => buf.push('-'),
                _ if eof => {
                    tokens.push(Token::Comment(core::mem::take(&mut buf)));
                    tokens.push(Token::Eof);
                    break;
                }
                _ => {
                    buf.push('-');
                    buf.push('-');
                    state = State::Comment;
                    continue;
                }
            },
            State::Doctype => match c {
                ' ' | '\t' | '\n' | '\r' | '\u{0C}' => state = State::BeforeDoctypeName,
                _ if eof => {
                    tokens.push(Token::Doctype { name: String::new(), force_quirks: true });
                    tokens.push(Token::Eof);
                    break;
                }
                _ => {
                    state = State::BeforeDoctypeName;
                    continue;
                }
            },
            State::BeforeDoctypeName => match c {
                ' ' | '\t' | '\n' | '\r' | '\u{0C}' => {}
                '>' => {
                    tokens.push(Token::Doctype { name: String::new(), force_quirks: true });
                    state = State::Data;
                }
                _ if eof => {
                    tokens.push(Token::Doctype { name: String::new(), force_quirks: true });
                    tokens.push(Token::Eof);
                    break;
                }
                _ => {
                    buf.clear();
                    buf.push(c.to_ascii_lowercase());
                    state = State::DoctypeName;
                }
            },
            State::DoctypeName => match c {
                ' ' | '\t' | '\n' | '\r' | '\u{0C}' => state = State::AfterDoctypeName,
                '>' => {
                    tokens.push(Token::Doctype { name: core::mem::take(&mut buf), force_quirks: false });
                    state = State::Data;
                }
                _ if eof => {
                    tokens.push(Token::Doctype { name: core::mem::take(&mut buf), force_quirks: true });
                    tokens.push(Token::Eof);
                    break;
                }
                _ => buf.push(c.to_ascii_lowercase()),
            },
            State::AfterDoctypeName => match c {
                ' ' | '\t' | '\n' | '\r' | '\u{0C}' => {}
                '>' => {
                    tokens.push(Token::Doctype { name: core::mem::take(&mut buf), force_quirks: false });
                    state = State::Data;
                }
                _ if eof => {
                    tokens.push(Token::Doctype { name: core::mem::take(&mut buf), force_quirks: true });
                    tokens.push(Token::Eof);
                    break;
                }
                // PUBLIC/SYSTEM-id's worden niet bewaard; sla ze over tot '>'.
                _ => state = State::BogusDoctype,
            },
            State::BogusDoctype => {
                if c == '>' {
                    tokens.push(Token::Doctype { name: core::mem::take(&mut buf), force_quirks: false });
                    state = State::Data;
                } else if eof {
                    tokens.push(Token::Doctype { name: core::mem::take(&mut buf), force_quirks: false });
                    tokens.push(Token::Eof);
                    break;
                }
            }
        }
        pos += 1;
    }

    if !matches!(tokens.last(), Some(Token::Eof)) {
        tokens.push(Token::Eof);
    }
    tokens
}
