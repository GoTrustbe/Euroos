//! EuroWeb — de soevereine browser-engine van EuroOS (Spoor B, zie
//! `docs/EUROBROWSER-PLAN.md`).
//!
//! Van scratch in Rust, `no_std`, geen foreign engine, geen ICU/NSS. Dit is de
//! **fundament-laag**: HTML5-tokenizer + tree-construction → DOM. Daarop volgen in
//! latere sprints CSS (cascade/selectors), layout (block/inline/flex) en paint naar
//! de EuroDisplay-framebuffer; JavaScript komt als tree-walking interpreter met
//! per-tab EuroGuard-capabilities.
//!
//! Architectuur-keuzes:
//! - **Eén `Vec<Node>`-arena** voor de DOM (geen `Rc`/`RefCell`), `#![forbid(unsafe_code)]`.
//! - **Spec-getrouwe tokenizer-toestandsmachine** (WHATWG), inclusief RAWTEXT/RCDATA
//!   en character references — host-getest tegen HTML5lib-achtige gevallen.
//! - **Pragmatische tree-construction** (open-element-stapel, void-elementen,
//!   impliciet sluiten) — genoeg voor statische pagina's; de volledige
//!   insertion-mode-machine is een latere verfijning.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod css;
pub mod dom;
pub mod entities;
pub mod flex;
pub mod layout;
pub mod paint;
pub mod parser;
pub mod tokenizer;

pub use css::{compute, parse_stylesheet, ComputedStyle, Rule, Selector, Stylesheet};
pub use dom::{Attr, Dom, Node, NodeId, NodeKind};
pub use layout::{layout, layout_with, BoxType, Dimensions, LayoutBox, Rect};
pub use paint::{paint, parse_color, DisplayItem};
pub use parser::parse;
pub use tokenizer::{tokenize, Token};

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::String;
    use alloc::vec::Vec;

    fn chars_of(tokens: &[Token]) -> String {
        tokens
            .iter()
            .filter_map(|t| match t {
                Token::Character(c) => Some(*c),
                _ => None,
            })
            .collect()
    }

    // ---- Tokenizer ----

    #[test]
    fn tokenize_simple_tag_and_text() {
        let t = tokenize("<p>Hallo</p>");
        assert_eq!(t[0], Token::StartTag { name: "p".into(), attrs: Vec::new(), self_closing: false });
        assert_eq!(chars_of(&t), "Hallo");
        assert_eq!(t[t.len() - 2], Token::EndTag { name: "p".into() });
        assert_eq!(t[t.len() - 1], Token::Eof);
    }

    #[test]
    fn tokenize_attributes_all_quoting_styles() {
        let t = tokenize(r#"<a href="x" title='y' data=z disabled>"#);
        if let Token::StartTag { name, attrs, .. } = &t[0] {
            assert_eq!(name, "a");
            assert_eq!(attrs.len(), 4);
            assert_eq!(attrs[0], Attr { name: "href".into(), value: "x".into() });
            assert_eq!(attrs[1], Attr { name: "title".into(), value: "y".into() });
            assert_eq!(attrs[2], Attr { name: "data".into(), value: "z".into() });
            assert_eq!(attrs[3], Attr { name: "disabled".into(), value: String::new() });
        } else {
            panic!("verwachtte StartTag, kreeg {:?}", t[0]);
        }
    }

    #[test]
    fn tokenize_self_closing_and_case_fold() {
        let t = tokenize("<IMG SRC='a.png'/>");
        assert_eq!(
            t[0],
            Token::StartTag {
                name: "img".into(),
                attrs: alloc::vec![Attr { name: "src".into(), value: "a.png".into() }],
                self_closing: true
            }
        );
    }

    #[test]
    fn tokenize_comment_and_doctype() {
        let t = tokenize("<!DOCTYPE html><!-- hoi -->");
        assert_eq!(t[0], Token::Doctype { name: "html".into(), force_quirks: false });
        assert_eq!(t[1], Token::Comment(" hoi ".into()));
    }

    #[test]
    fn tokenize_entities_in_text() {
        let t = tokenize("a &amp; b &lt;tag&gt; &#169; &#x20AC; &euro;");
        assert_eq!(chars_of(&t), "a & b <tag> © € €");
    }

    #[test]
    fn tokenize_entity_without_semicolon_legacy() {
        // &copy zonder ; is een legacy entiteit in tekst.
        let t = tokenize("\u{A9}: &copy 2026");
        assert_eq!(chars_of(&t), "©: © 2026");
    }

    #[test]
    fn tokenize_ambiguous_ampersand_left_literal() {
        // &notanentity; → '&' blijft letterlijk (geen match).
        let t = tokenize("x &zzz; y");
        assert_eq!(chars_of(&t), "x &zzz; y");
    }

    #[test]
    fn tokenize_rawtext_script_keeps_markup() {
        let t = tokenize("<script>if (a<b && c>d) x()</script>after");
        // Binnen <script> blijft '<b' tekst; pas </script> sluit af.
        let txt = chars_of(&t);
        assert!(txt.contains("if (a<b && c>d) x()"), "script-inhoud verloren: {txt:?}");
        assert!(txt.ends_with("after"));
        assert!(t.iter().any(|x| *x == Token::EndTag { name: "script".into() }));
        // Géén losse <b>-starttag binnen de script-rawtext.
        assert!(!t.iter().any(|x| matches!(x, Token::StartTag { name, .. } if name == "b")));
    }

    #[test]
    fn tokenize_rcdata_title_decodes_entities() {
        let t = tokenize("<title>A &amp; B</title>");
        assert_eq!(chars_of(&t), "A & B");
    }

    #[test]
    fn tokenize_duplicate_attr_first_wins() {
        let t = tokenize(r#"<x a="1" a="2">"#);
        if let Token::StartTag { attrs, .. } = &t[0] {
            assert_eq!(attrs.len(), 1);
            assert_eq!(attrs[0].value, "1");
        } else {
            panic!();
        }
    }

    // ---- DOM / parser ----

    #[test]
    fn parse_nested_structure() {
        let dom = parse("<html><body><h1>Titel</h1><p>Tekst</p></body></html>");
        assert_eq!(dom.count_tag("html"), 1);
        assert_eq!(dom.count_tag("body"), 1);
        assert_eq!(dom.count_tag("h1"), 1);
        assert_eq!(dom.count_tag("p"), 1);
        assert_eq!(dom.text_content(dom.root()), "TitelTekst");
    }

    #[test]
    fn parse_implicit_p_close() {
        // <p>a<p>b → twee zelfstandige paragrafen, niet genest.
        let dom = parse("<p>a<p>b");
        assert_eq!(dom.count_tag("p"), 2);
        // Geen p binnen een p.
        for (i, n) in dom.nodes.iter().enumerate() {
            if dom.tag(i) == Some("p") {
                for &c in &n.children {
                    assert_ne!(dom.tag(c), Some("p"), "p genest in p");
                }
            }
        }
    }

    #[test]
    fn parse_list_items_autoclose() {
        let dom = parse("<ul><li>een<li>twee<li>drie</ul>");
        assert_eq!(dom.count_tag("li"), 3);
        // Elke li is direct kind van ul (niet genest).
        let ul = (0..dom.len()).find(|&i| dom.tag(i) == Some("ul")).unwrap();
        let li_children = dom.nodes[ul].children.iter().filter(|&&c| dom.tag(c) == Some("li")).count();
        assert_eq!(li_children, 3);
    }

    #[test]
    fn parse_void_elements_have_no_children() {
        let dom = parse("<div><img src='a'><br>tekst</div>");
        let img = (0..dom.len()).find(|&i| dom.tag(i) == Some("img")).unwrap();
        assert!(dom.nodes[img].children.is_empty());
        // 'tekst' hangt onder div, niet onder br.
        let div = (0..dom.len()).find(|&i| dom.tag(i) == Some("div")).unwrap();
        assert_eq!(dom.text_content(div), "tekst");
    }

    #[test]
    fn parse_attributes_reachable() {
        let dom = parse(r#"<a href="https://euro-os.eu" class="btn">link</a>"#);
        let a = (0..dom.len()).find(|&i| dom.tag(i) == Some("a")).unwrap();
        assert_eq!(dom.attr(a, "href"), Some("https://euro-os.eu"));
        assert_eq!(dom.attr(a, "class"), Some("btn"));
        assert_eq!(dom.text_content(a), "link");
    }

    #[test]
    fn parse_mismatched_end_tag_ignored() {
        // </span> zonder open span mag de boom niet breken.
        let dom = parse("<div>x</span>y</div>");
        assert_eq!(dom.count_tag("div"), 1);
        let div = (0..dom.len()).find(|&i| dom.tag(i) == Some("div")).unwrap();
        assert_eq!(dom.text_content(div), "xy");
    }

    #[test]
    fn parse_realistic_fragment() {
        let html = r#"
            <!DOCTYPE html>
            <html lang="nl">
              <head><meta charset="utf-8"><title>EuroOS</title></head>
              <body>
                <header><h1>EuroOS &mdash; soeverein</h1></header>
                <main>
                  <p class="lead">Van nul gebouwd in <strong>Rust</strong>.</p>
                  <ul><li>HTML</li><li>CSS</li><li>Layout</li></ul>
                </main>
                <!-- footer komt later -->
              </body>
            </html>"#;
        let dom = parse(html);
        assert_eq!(dom.count_tag("html"), 1);
        assert_eq!(dom.count_tag("title"), 1);
        assert_eq!(dom.count_tag("li"), 3);
        assert_eq!(dom.count_tag("strong"), 1);
        let h1 = (0..dom.len()).find(|&i| dom.tag(i) == Some("h1")).unwrap();
        assert_eq!(dom.text_content(h1), "EuroOS — soeverein");
        let html_el = (0..dom.len()).find(|&i| dom.tag(i) == Some("html")).unwrap();
        assert_eq!(dom.attr(html_el, "lang"), Some("nl"));
    }

    // ---- Robuustheid / stabiliteit: kwaadwillige & misvormde invoer ----
    //
    // De engine draait in de kernel; ZIJ MAG NOOIT crashen op slechte invoer.
    // Deze tests jagen de VOLLEDIGE pijplijn (parse → compute → layout → paint)
    // door pathologische pagina's en eisen: geen paniek, begrensde uitvoer.

    fn full_pipeline(html: &str, css: &str) -> usize {
        let dom = parse(html);
        let sheet = parse_stylesheet(css);
        let styles = compute(&dom, &[&sheet]);
        let root = layout(&dom, &styles, 1280.0);
        let items = paint(&dom, &styles, &root);
        items.len()
    }

    /// Diep geneste `<div>` (zou zonder diepte-grens de kernel-stack opblazen).
    #[test]
    fn robust_deeply_nested_does_not_overflow() {
        let mut html = String::new();
        for _ in 0..20_000 {
            html.push_str("<div>");
        }
        html.push_str("diep");
        for _ in 0..20_000 {
            html.push_str("</div>");
        }
        // Mag niet paniceren/overflowen; de layout-boom is begrensd op MAX_DEPTH.
        let _ = full_pipeline(&html, "div{color:#222;padding:1px}");
        // De DOM bevat wél alle knopen (parser is iteratief), maar build_box
        // daalt niet dieper dan MAX_DEPTH af.
        let dom = parse(&html);
        assert!(dom.len() >= 20_000, "parser moet alle knopen aanmaken");
    }

    /// Diep geneste tekst-knoop via text_content (eigen diepte-grens).
    #[test]
    fn robust_text_content_deep_no_overflow() {
        let mut html = String::new();
        for _ in 0..5_000 {
            html.push_str("<span>");
        }
        html.push('x');
        let dom = parse(&html);
        let root = 0;
        let _ = dom.text_content(root); // mag niet overflowen
    }

    /// Misvormde / niet-gesloten / rommel-markup.
    #[test]
    fn robust_malformed_markup() {
        let cases = [
            "<<<>>><p<<<",
            "<div class=\"unterminated",
            "<p>tekst</nonexistent></p></p></p>",
            "<a href=>&&&;&#;&#x;&zzz;",
            "</div></div></div>",
            "<!-- niet gesloten comment",
            "<script>onafgesloten",
            "<style>body{color: ; ;;; } broken",
            "",
            "<>",
            "&#999999999;&#xFFFFFFFF;", // out-of-range code points
        ];
        for c in cases {
            let _ = full_pipeline(c, "* { margin: 1px }");
        }
    }

    /// Misvormde CSS mag de cascade niet laten crashen.
    #[test]
    fn robust_malformed_css() {
        let css = "}}}{{{ : ; color color color ;; @媒体 { x } .a..b###{} div > > p {} *{padding:-99999px}";
        let _ = full_pipeline("<div class='a'><p>hi</p></div>", css);
    }

    /// Heel veel siblings (breedte i.p.v. diepte) — begrensde, maar grote, invoer.
    #[test]
    fn robust_many_siblings() {
        let mut html = String::from("<body>");
        for i in 0..10_000 {
            html.push_str("<p>item ");
            // varieer de inhoud zodat het geen identieke strings zijn
            html.push_str(if i % 2 == 0 { "even" } else { "oneven" });
            html.push_str("</p>");
        }
        html.push_str("</body>");
        let n = full_pipeline(&html, "p{font-size:14px}");
        assert!(n > 0, "moet display-items opleveren");
    }

    /// Lange ongesplitste tekst en rare unicode mag niet paniceren.
    #[test]
    fn robust_long_unicode_text() {
        let mut html = String::from("<p>");
        for _ in 0..2_000 {
            html.push_str("woord\u{200B}\u{00A0}€中文🇪🇺 ");
        }
        html.push_str("</p>");
        let _ = full_pipeline(&html, "p{}");
    }
}
