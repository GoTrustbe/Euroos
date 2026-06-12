//! EuroWeb CSS-engine (Sprint AB-B2): parser + selector-matching + cascade.
//!
//! Ontleedt een stylesheet naar regels (selector-lijst + declaraties), matcht
//! selectoren tegen de [`Dom`] met **specificiteit**, en berekent per knoop de
//! **computed style** via de cascade (UA- vóór author-herkomst, specificiteit,
//! bronvolgorde, `!important`) plus **overerving** van overgeërfde eigenschappen
//! en inline `style`-attributen. Pure `no_std`-logica, host-getest.
//!
//! Ondersteunde selectoren: type (`div`), universeel (`*`), class (`.x`), id
//! (`#y`), samengesteld (`div.x#y`), en de combinators descendant (`a b`) en
//! child (`a > b`), inclusief selector-lijsten (`a, b`). Genoeg voor het echte
//! web; pseudo-classes/attribuut-selectoren zijn een latere verfijning.

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::dom::{Dom, NodeId, NodeKind};

/// Eén CSS-declaratie (`naam: waarde` met optioneel `!important`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declaration {
    pub name: String,
    pub value: String,
    pub important: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Combinator {
    Descendant,
    Child,
}

/// Een samengestelde selector: optioneel type, id en classes.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct Compound {
    tag: Option<String>,
    id: Option<String>,
    classes: Vec<String>,
}

/// Een volledige selector: een rij samengestelde selectoren met combinators.
#[derive(Debug, Clone)]
pub struct Selector {
    compounds: Vec<Compound>,
    /// `combinators[i]` staat tussen `compounds[i]` en `compounds[i+1]`.
    combinators: Vec<Combinator>,
}

/// Specificiteit als (id's, classes, types). Hoger = sterker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Specificity(pub u32, pub u32, pub u32);

impl Selector {
    /// CSS-specificiteit: tel id's, classes en types over alle compounds.
    pub fn specificity(&self) -> Specificity {
        let mut a = 0;
        let mut b = 0;
        let mut c = 0;
        for comp in &self.compounds {
            if comp.id.is_some() {
                a += 1;
            }
            b += comp.classes.len() as u32;
            if comp.tag.is_some() {
                c += 1;
            }
        }
        Specificity(a, b, c)
    }

    /// Matcht deze selector de knoop `node` in `dom`?
    pub fn matches(&self, dom: &Dom, node: NodeId) -> bool {
        let last = self.compounds.len() - 1;
        if !compound_matches(&self.compounds[last], dom, node) {
            return false;
        }
        let mut current = node;
        let mut i = last;
        while i > 0 {
            let comb = self.combinators[i - 1];
            let want = &self.compounds[i - 1];
            match comb {
                Combinator::Child => {
                    match dom.nodes[current].parent {
                        Some(p) if compound_matches(want, dom, p) => current = p,
                        _ => return false,
                    }
                }
                Combinator::Descendant => {
                    let mut anc = dom.nodes[current].parent;
                    let mut found = None;
                    while let Some(p) = anc {
                        if compound_matches(want, dom, p) {
                            found = Some(p);
                            break;
                        }
                        anc = dom.nodes[p].parent;
                    }
                    match found {
                        Some(p) => current = p,
                        None => return false,
                    }
                }
            }
            i -= 1;
        }
        true
    }
}

fn compound_matches(c: &Compound, dom: &Dom, node: NodeId) -> bool {
    let tag = match dom.tag(node) {
        Some(t) => t,
        None => return false, // alleen elementen matchen
    };
    if let Some(want) = &c.tag {
        if want != tag {
            return false;
        }
    }
    if let Some(want_id) = &c.id {
        if dom.attr(node, "id") != Some(want_id.as_str()) {
            return false;
        }
    }
    if !c.classes.is_empty() {
        let class_attr = dom.attr(node, "class").unwrap_or("");
        for want in &c.classes {
            if !class_attr.split_ascii_whitespace().any(|cl| cl == want) {
                return false;
            }
        }
    }
    true
}

/// Eén CSS-regel: een lijst selectoren met een gedeelde set declaraties.
#[derive(Debug, Clone)]
pub struct Rule {
    pub selectors: Vec<Selector>,
    pub declarations: Vec<Declaration>,
}

/// Een geparste stylesheet.
#[derive(Debug, Clone, Default)]
pub struct Stylesheet {
    pub rules: Vec<Rule>,
}

/// Verwijder `/* ... */`-commentaar.
fn strip_comments(input: &str) -> String {
    let b: Vec<char> = input.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == '/' && i + 1 < b.len() && b[i + 1] == '*' {
            i += 2;
            while i + 1 < b.len() && !(b[i] == '*' && b[i + 1] == '/') {
                i += 1;
            }
            i += 2;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    out
}

fn parse_compound(s: &str) -> Option<Compound> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let mut comp = Compound::default();
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    // Optioneel leidend type of '*'.
    if chars[i].is_ascii_alphabetic() {
        let start = i;
        while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '-') {
            i += 1;
        }
        comp.tag = Some(chars[start..i].iter().collect::<String>().to_ascii_lowercase());
    } else if chars[i] == '*' {
        i += 1; // universeel: geen type-constraint
    }
    // Daarna #id en .class in willekeurige volgorde.
    while i < chars.len() {
        match chars[i] {
            '#' => {
                i += 1;
                let start = i;
                while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '-' || chars[i] == '_') {
                    i += 1;
                }
                comp.id = Some(chars[start..i].iter().collect());
            }
            '.' => {
                i += 1;
                let start = i;
                while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '-' || chars[i] == '_') {
                    i += 1;
                }
                comp.classes.push(chars[start..i].iter().collect());
            }
            _ => return None, // niet-ondersteund teken
        }
    }
    Some(comp)
}

fn parse_selector(s: &str) -> Option<Selector> {
    // Tokeniseer op whitespace en '>'.
    let mut compounds = Vec::new();
    let mut combinators = Vec::new();
    let mut pending_child = false;
    let mut first = true;
    for tok in s.split_ascii_whitespace() {
        if tok == ">" {
            pending_child = true;
            continue;
        }
        // '>' kan ook aan een compound vastgeplakt zitten (a>b); splits dat.
        for (k, part) in tok.split('>').enumerate() {
            if part.is_empty() {
                pending_child = true;
                continue;
            }
            let comp = parse_compound(part)?;
            if first {
                first = false;
            } else {
                combinators.push(if pending_child || k > 0 {
                    Combinator::Child
                } else {
                    Combinator::Descendant
                });
            }
            compounds.push(comp);
            pending_child = false;
        }
    }
    if compounds.is_empty() {
        return None;
    }
    Some(Selector { compounds, combinators })
}

fn parse_declarations(block: &str) -> Vec<Declaration> {
    let mut decls = Vec::new();
    for chunk in block.split(';') {
        let chunk = chunk.trim();
        if chunk.is_empty() {
            continue;
        }
        if let Some(colon) = chunk.find(':') {
            let name = chunk[..colon].trim().to_ascii_lowercase();
            let mut value = chunk[colon + 1..].trim().to_string();
            let important = value.to_ascii_lowercase().ends_with("!important");
            if important {
                // verwijder de !important-suffix
                let cut = value.to_ascii_lowercase().rfind("!important").unwrap();
                value = value[..cut].trim().to_string();
            }
            if !name.is_empty() && !value.is_empty() {
                decls.push(Declaration { name, value, important });
            }
        }
    }
    decls
}

/// Parse een stylesheet-string.
pub fn parse_stylesheet(input: &str) -> Stylesheet {
    let src = strip_comments(input);
    let mut rules = Vec::new();
    let bytes: Vec<char> = src.chars().collect();
    let mut i = 0;
    while i < bytes.len() {
        // Lees selector-tekst tot '{'.
        let sel_start = i;
        while i < bytes.len() && bytes[i] != '{' && bytes[i] != '}' {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] == '}' {
            break;
        }
        let sel_text: String = bytes[sel_start..i].iter().collect();
        i += 1; // sla '{' over
        let blk_start = i;
        while i < bytes.len() && bytes[i] != '}' {
            i += 1;
        }
        let block: String = bytes[blk_start..i].iter().collect();
        if i < bytes.len() {
            i += 1; // sla '}' over
        }
        // At-regels (@media, @import, ...) slaan we voorlopig over.
        if sel_text.trim_start().starts_with('@') {
            continue;
        }
        let selectors: Vec<Selector> = sel_text
            .split(',')
            .filter_map(|s| parse_selector(s.trim()))
            .collect();
        if selectors.is_empty() {
            continue;
        }
        let declarations = parse_declarations(&block);
        if declarations.is_empty() {
            continue;
        }
        rules.push(Rule { selectors, declarations });
    }
    Stylesheet { rules }
}

/// De berekende stijl van één knoop: eigenschap → waarde.
pub type ComputedStyle = BTreeMap<String, String>;

/// Eigenschappen die overerven van ouder naar kind (CSS-standaardset, subset).
fn is_inherited(prop: &str) -> bool {
    matches!(
        prop,
        "color"
            | "font"
            | "font-family"
            | "font-size"
            | "font-style"
            | "font-weight"
            | "line-height"
            | "letter-spacing"
            | "text-align"
            | "text-indent"
            | "text-transform"
            | "visibility"
            | "white-space"
            | "word-spacing"
            | "list-style"
            | "list-style-type"
            | "cursor"
            | "direction"
    )
}

struct Match {
    important: bool,
    spec: Specificity,
    order: u32,
    name: String,
    value: String,
}

/// De "sleutel" van een selector = de meest-specifieke simpele selector in de
/// rechtste compound (id > class > tag > universeel). Wordt gebruikt om regels te
/// indexeren zodat een knoop alleen relevante regels test (i.p.v. allemaal).
enum SelKey {
    Id(String),
    Class(String),
    Tag(String),
    Universal,
}

impl Selector {
    fn key(&self) -> SelKey {
        let last = &self.compounds[self.compounds.len() - 1];
        if let Some(id) = &last.id {
            SelKey::Id(id.clone())
        } else if let Some(c) = last.classes.first() {
            SelKey::Class(c.clone())
        } else if let Some(t) = &last.tag {
            SelKey::Tag(t.clone())
        } else {
            SelKey::Universal
        }
    }
}

/// Bereken de computed style per knoop, gegeven stylesheets in cascade-volgorde
/// (UA eerst, daarna author). Past matching, specificiteit, bronvolgorde,
/// `!important`, inline `style`-attributen en overerving toe.
///
/// PRESTATIE: regels worden geïndexeerd op hun sleutel-selector (id/class/tag),
/// zodat elke knoop slechts een handvol kandidaat-regels test i.p.v. álle regels.
/// Dit maakt het verschil tussen O(knopen × regels) en O(knopen × weinig) — cruciaal
/// voor echte websites met duizenden elementen en honderden regels.
pub fn compute(dom: &Dom, sheets: &[&Stylesheet]) -> Vec<ComputedStyle> {
    let n = dom.len();
    let mut out: Vec<ComputedStyle> = Vec::with_capacity(n);
    for _ in 0..n {
        out.push(ComputedStyle::new());
    }

    // Vlak alle regels af in cascade-volgorde + bereken per regel de order-basis
    // (cumulatief aantal declaraties ervóór) zodat de bronvolgorde bewaard blijft.
    let mut flat: Vec<&Rule> = Vec::new();
    for s in sheets {
        for r in &s.rules {
            flat.push(r);
        }
    }
    let mut order_base: Vec<u32> = Vec::with_capacity(flat.len());
    let mut acc = 0u32;
    for r in &flat {
        order_base.push(acc);
        acc = acc.saturating_add(r.declarations.len() as u32);
    }

    // Index: regel-indices per id / class / tag, plus universele regels.
    let mut by_id: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    let mut by_class: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    let mut by_tag: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    let mut universal: Vec<usize> = Vec::new();
    for (ri, r) in flat.iter().enumerate() {
        for sel in &r.selectors {
            match sel.key() {
                SelKey::Id(s) => by_id.entry(s).or_default().push(ri),
                SelKey::Class(s) => by_class.entry(s).or_default().push(ri),
                SelKey::Tag(s) => by_tag.entry(s).or_default().push(ri),
                SelKey::Universal => universal.push(ri),
            }
        }
    }

    // Document-volgorde = oplopende NodeId → ouders vóór kinderen → overerving werkt.
    for node in 0..n {
        // 1) Start met overgeërfde eigenschappen van de ouder.
        let mut style = ComputedStyle::new();
        if let Some(parent) = dom.nodes[node].parent {
            for (k, v) in &out[parent] {
                if is_inherited(k) {
                    style.insert(k.clone(), v.clone());
                }
            }
        }

        if matches!(dom.nodes[node].kind, NodeKind::Element { .. }) {
            // 2) Verzamel kandidaat-regels via de index (universeel + id + classes + tag).
            let mut cands: Vec<usize> = universal.clone();
            if let Some(idv) = dom.attr(node, "id") {
                if let Some(v) = by_id.get(idv) {
                    cands.extend_from_slice(v);
                }
            }
            if let Some(clsv) = dom.attr(node, "class") {
                for c in clsv.split_ascii_whitespace() {
                    if let Some(v) = by_class.get(c) {
                        cands.extend_from_slice(v);
                    }
                }
            }
            if let Some(tag) = dom.tag(node) {
                if let Some(v) = by_tag.get(tag) {
                    cands.extend_from_slice(v);
                }
            }
            cands.sort_unstable();
            cands.dedup();

            // 3) Test alleen de kandidaten; bewaar gematchte declaraties + order.
            let mut matches: Vec<Match> = Vec::new();
            for &ri in &cands {
                let rule = flat[ri];
                let mut best: Option<Specificity> = None;
                for sel in &rule.selectors {
                    if sel.matches(dom, node) {
                        let s = sel.specificity();
                        best = Some(best.map_or(s, |b| if s > b { s } else { b }));
                    }
                }
                if let Some(spec) = best {
                    for (j, d) in rule.declarations.iter().enumerate() {
                        matches.push(Match {
                            important: d.important,
                            spec,
                            order: order_base[ri].saturating_add(j as u32),
                            name: d.name.clone(),
                            value: d.value.clone(),
                        });
                    }
                }
            }

            // 4) Inline style="" — hoogste author-prioriteit.
            if let Some(inline) = dom.attr(node, "style") {
                for d in parse_declarations(inline) {
                    matches.push(Match {
                        important: d.important,
                        spec: Specificity(1, 0, 0),
                        order: u32::MAX,
                        name: d.name,
                        value: d.value,
                    });
                }
            }

            // 5) Sorteer (!important > normaal, dan specificiteit, dan bronvolgorde) + pas toe.
            matches.sort_by(|x, y| {
                x.important
                    .cmp(&y.important)
                    .then(x.spec.cmp(&y.spec))
                    .then(x.order.cmp(&y.order))
            });
            for m in &matches {
                style.insert(m.name.clone(), m.value.clone());
            }
        }

        out[node] = style;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse;

    fn find_tag(dom: &Dom, tag: &str) -> NodeId {
        (0..dom.len()).find(|&i| dom.tag(i) == Some(tag)).unwrap()
    }

    #[test]
    fn parse_basic_rules() {
        let ss = parse_stylesheet("h1 { color: red; font-size: 24px } p, .lead { margin: 0 }");
        assert_eq!(ss.rules.len(), 2);
        assert_eq!(ss.rules[0].declarations.len(), 2);
        assert_eq!(ss.rules[1].selectors.len(), 2);
    }

    #[test]
    fn comments_stripped() {
        let ss = parse_stylesheet("/* x */ a { color: blue } /* y */");
        assert_eq!(ss.rules.len(), 1);
    }

    #[test]
    fn specificity_ordering() {
        let id = parse_selector("#x").unwrap().specificity();
        let cls = parse_selector(".x").unwrap().specificity();
        let typ = parse_selector("div").unwrap().specificity();
        let compound = parse_selector("div.x#y").unwrap().specificity();
        assert!(id > cls && cls > typ);
        assert_eq!(compound, Specificity(1, 1, 1));
    }

    #[test]
    fn selector_matching_type_class_id() {
        let dom = parse(r#"<div><p class="lead" id="first">x</p><p>y</p></div>"#);
        let p1 = find_tag(&dom, "p");
        assert!(parse_selector("p").unwrap().matches(&dom, p1));
        assert!(parse_selector(".lead").unwrap().matches(&dom, p1));
        assert!(parse_selector("#first").unwrap().matches(&dom, p1));
        assert!(parse_selector("p.lead#first").unwrap().matches(&dom, p1));
        assert!(!parse_selector("a").unwrap().matches(&dom, p1));
    }

    #[test]
    fn descendant_and_child_combinators() {
        let dom = parse("<article><section><p>x</p></section></article>");
        let p = find_tag(&dom, "p");
        assert!(parse_selector("article p").unwrap().matches(&dom, p)); // descendant
        assert!(parse_selector("section > p").unwrap().matches(&dom, p)); // direct child
        assert!(!parse_selector("article > p").unwrap().matches(&dom, p)); // niet direct kind
    }

    #[test]
    fn cascade_specificity_wins() {
        let dom = parse(r#"<p class="lead" id="x">hi</p>"#);
        let ss = parse_stylesheet(
            "p { color: black } .lead { color: green } #x { color: red }",
        );
        let styles = compute(&dom, &[&ss]);
        let p = find_tag(&dom, "p");
        assert_eq!(styles[p].get("color").map(|s| s.as_str()), Some("red"));
    }

    #[test]
    fn cascade_source_order_breaks_ties() {
        let dom = parse(r#"<p class="a b">hi</p>"#);
        let ss = parse_stylesheet(".a { color: blue } .b { color: orange }");
        let styles = compute(&dom, &[&ss]);
        let p = find_tag(&dom, "p");
        // Gelijke specificiteit → laatste regel wint.
        assert_eq!(styles[p].get("color").map(|s| s.as_str()), Some("orange"));
    }

    #[test]
    fn important_beats_specificity() {
        let dom = parse(r#"<p id="x">hi</p>"#);
        let ss = parse_stylesheet("p { color: green !important } #x { color: red }");
        let styles = compute(&dom, &[&ss]);
        let p = find_tag(&dom, "p");
        assert_eq!(styles[p].get("color").map(|s| s.as_str()), Some("green"));
    }

    #[test]
    fn inline_style_beats_stylesheet() {
        let dom = parse(r#"<p style="color: purple">hi</p>"#);
        let ss = parse_stylesheet("p { color: red }");
        let styles = compute(&dom, &[&ss]);
        let p = find_tag(&dom, "p");
        assert_eq!(styles[p].get("color").map(|s| s.as_str()), Some("purple"));
    }

    #[test]
    fn ua_before_author_order() {
        let dom = parse("<p>hi</p>");
        let ua = parse_stylesheet("p { color: black }");
        let author = parse_stylesheet("p { color: navy }");
        let styles = compute(&dom, &[&ua, &author]);
        let p = find_tag(&dom, "p");
        // Auteur staat later in de cascade → wint bij gelijke specificiteit.
        assert_eq!(styles[p].get("color").map(|s| s.as_str()), Some("navy"));
    }

    #[test]
    fn inheritance_of_color() {
        let dom = parse("<div><p>hi <em>daar</em></p></div>");
        let ss = parse_stylesheet("div { color: teal; border: 1px }");
        let styles = compute(&dom, &[&ss]);
        let em = find_tag(&dom, "em");
        // 'color' erft door tot <em>; 'border' (niet-overgeërfd) niet.
        assert_eq!(styles[em].get("color").map(|s| s.as_str()), Some("teal"));
        assert_eq!(styles[em].get("border"), None);
    }
}
