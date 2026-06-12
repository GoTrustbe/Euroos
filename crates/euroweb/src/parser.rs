//! EuroWeb tree-construction — bouwt een [`Dom`] uit de token-stroom.
//!
//! Een pragmatische maar echte boom-bouwer: een stapel van open elementen, void-
//! elementen die geen kinderen krijgen, impliciet sluiten van `<p>`/`<li>`/tabel-
//! cellen, samenvoegen van opeenvolgende tekst, en het negeren van niet-passende
//! eindtags (zoals browsers doen). Dit is genoeg om statische pagina's correct te
//! structureren; de volledige WHATWG "insertion mode"-machine is een latere
//! verfijning (zie docs/EUROBROWSER-PLAN.md, fase B1).

use alloc::string::String;
use alloc::vec::Vec;

use crate::dom::{Attr, Dom, NodeId, NodeKind};
use crate::tokenizer::{tokenize, Token};

/// Elementen zonder eindtag/inhoud.
fn is_void(tag: &str) -> bool {
    matches!(
        tag,
        "area" | "base" | "br" | "col" | "embed" | "hr" | "img" | "input"
            | "link" | "meta" | "param" | "source" | "track" | "wbr"
    )
}

/// Block-/flow-elementen die een open `<p>` impliciet sluiten.
fn closes_p(tag: &str) -> bool {
    matches!(
        tag,
        "address" | "article" | "aside" | "blockquote" | "details" | "div"
            | "dl" | "fieldset" | "figcaption" | "figure" | "footer" | "form"
            | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "header" | "hr"
            | "main" | "menu" | "nav" | "ol" | "p" | "pre" | "section"
            | "table" | "ul"
    )
}

struct Builder {
    dom: Dom,
    stack: Vec<NodeId>,
}

impl Builder {
    fn new() -> Self {
        let dom = Dom::new();
        let root = dom.root();
        Builder { dom, stack: alloc::vec![root] }
    }

    fn current(&self) -> NodeId {
        *self.stack.last().unwrap_or(&0)
    }

    /// Is een element met deze tagnaam open op de stapel?
    fn in_scope(&self, tag: &str) -> bool {
        self.stack.iter().rev().any(|&id| self.dom.tag(id) == Some(tag))
    }

    /// Sluit (pop) tot en met het dichtstbijzijnde open element met deze tagnaam.
    fn close_to(&mut self, tag: &str) {
        if !self.in_scope(tag) {
            return;
        }
        while let Some(&top) = self.stack.last() {
            let t = self.dom.tag(top).map(String::from);
            self.stack.pop();
            if t.as_deref() == Some(tag) {
                break;
            }
        }
    }

    /// Impliciete sluitingsregels vóór het invoegen van `tag`.
    fn handle_implicit_close(&mut self, tag: &str) {
        match tag {
            "li" => {
                if self.dom.tag(self.current()) == Some("li") {
                    self.close_to("li");
                }
            }
            "dd" | "dt" => {
                if matches!(self.dom.tag(self.current()), Some("dd") | Some("dt")) {
                    let cur = String::from(self.dom.tag(self.current()).unwrap());
                    self.close_to(&cur);
                }
            }
            "option" => {
                if self.dom.tag(self.current()) == Some("option") {
                    self.close_to("option");
                }
            }
            "tr" => {
                for cell in ["td", "th"] {
                    if self.dom.tag(self.current()) == Some(cell) {
                        self.close_to(cell);
                    }
                }
                if self.dom.tag(self.current()) == Some("tr") {
                    self.close_to("tr");
                }
            }
            "td" | "th" => {
                for cell in ["td", "th"] {
                    if self.dom.tag(self.current()) == Some(cell) {
                        self.close_to(cell);
                    }
                }
            }
            _ if closes_p(tag) => {
                if self.in_scope("p") && self.dom.tag(self.current()) == Some("p") {
                    self.close_to("p");
                }
            }
            _ => {}
        }
    }

    fn insert_text(&mut self, s: &str) {
        let parent = self.current();
        // Voeg samen met een direct voorafgaande tekst-knoop.
        if let Some(&last) = self.dom.nodes[parent].children.last() {
            if let NodeKind::Text(t) = &mut self.dom.nodes[last].kind {
                t.push_str(s);
                return;
            }
        }
        self.dom.append(parent, NodeKind::Text(String::from(s)));
    }

    fn run(mut self, tokens: Vec<Token>) -> Dom {
        let mut pending_text = String::new();

        macro_rules! flush_text {
            () => {
                if !pending_text.is_empty() {
                    let s = core::mem::take(&mut pending_text);
                    self.insert_text(&s);
                }
            };
        }

        for tok in tokens {
            match tok {
                Token::Character(c) => pending_text.push(c),
                Token::StartTag { name, attrs, self_closing } => {
                    flush_text!();
                    self.handle_implicit_close(&name);
                    let parent = self.current();
                    let attrs: Vec<Attr> = attrs;
                    let void = is_void(&name);
                    let id = self.dom.append(parent, NodeKind::Element { name, attrs });
                    if !void && !self_closing {
                        self.stack.push(id);
                    }
                }
                Token::EndTag { name } => {
                    flush_text!();
                    if is_void(&name) {
                        continue; // </br> e.d. negeren
                    }
                    if self.in_scope(&name) {
                        self.close_to(&name);
                    }
                }
                Token::Comment(text) => {
                    flush_text!();
                    let parent = self.current();
                    self.dom.append(parent, NodeKind::Comment(text));
                }
                Token::Doctype { name, .. } => {
                    flush_text!();
                    let root = self.dom.root();
                    self.dom.append(root, NodeKind::Doctype(name));
                }
                Token::Eof => {
                    flush_text!();
                    break;
                }
            }
        }
        self.dom
    }
}

/// Parse een HTML-string naar een [`Dom`]-boom.
pub fn parse(html: &str) -> Dom {
    let tokens = tokenize(html);
    Builder::new().run(tokens)
}
