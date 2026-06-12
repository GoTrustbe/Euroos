//! EuroAccess — de toegankelijkheidslaag van EuroDisplay (plan P2).
//!
//! Toegankelijkheid is in de EU een *aanbestedingsvereiste* (EN 301 549), geen extra.
//! Dit crate is het AT-SPI-equivalent: een **accessibility-boom** (rollen, namen,
//! toestanden), **focusbeheer** (volgende/vorige focusbare knoop in leesvolgorde) en
//! een **meertalige schermlezer** die elke knoop in de taal van de gebruiker
//! aankondigt — soeverein én toegankelijk, want de rol-labels komen uit EuroLocale.
//!
//! Pure, host-geteste `no_std`-logica; EuroDisplay vult de boom met echte widgets.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use eurolocale::Lang;

/// De rol van een UI-element (een subset van de ARIA/AT-SPI-rollen).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    Window,
    Heading,
    Label,
    Button,
    TextField,
    CheckBox,
    List,
    ListItem,
    Menu,
    MenuItem,
    Link,
}

impl Role {
    /// Is een element met deze rol standaard focusbaar (toetsenbordnavigatie)?
    fn focusable(self) -> bool {
        matches!(
            self,
            Role::Button | Role::TextField | Role::CheckBox | Role::ListItem | Role::MenuItem | Role::Link
        )
    }

    /// Het rol-label in de taal van de gebruiker (schermlezer spreekt EU-talen).
    pub fn label(self, lang: Lang) -> &'static str {
        use Lang::*;
        use Role::*;
        match (lang, self) {
            (Nl, Window) => "venster", (Nl, Heading) => "kop", (Nl, Label) => "label",
            (Nl, Button) => "knop", (Nl, TextField) => "tekstveld", (Nl, CheckBox) => "selectievakje",
            (Nl, List) => "lijst", (Nl, ListItem) => "lijstitem", (Nl, Menu) => "menu",
            (Nl, MenuItem) => "menu-item", (Nl, Link) => "koppeling",
            (De, Window) => "Fenster", (De, Heading) => "Überschrift", (De, Label) => "Beschriftung",
            (De, Button) => "Schaltfläche", (De, TextField) => "Textfeld", (De, CheckBox) => "Kontrollkästchen",
            (De, List) => "Liste", (De, ListItem) => "Listenelement", (De, Menu) => "Menü",
            (De, MenuItem) => "Menüpunkt", (De, Link) => "Link",
            (Fr, Window) => "fenêtre", (Fr, Heading) => "titre", (Fr, Label) => "étiquette",
            (Fr, Button) => "bouton", (Fr, TextField) => "champ de texte", (Fr, CheckBox) => "case à cocher",
            (Fr, List) => "liste", (Fr, ListItem) => "élément de liste", (Fr, Menu) => "menu",
            (Fr, MenuItem) => "élément de menu", (Fr, Link) => "lien",
            // Engelse fallback voor alle overige talen.
            (_, Window) => "window", (_, Heading) => "heading", (_, Label) => "label",
            (_, Button) => "button", (_, TextField) => "text field", (_, CheckBox) => "checkbox",
            (_, List) => "list", (_, ListItem) => "list item", (_, Menu) => "menu",
            (_, MenuItem) => "menu item", (_, Link) => "link",
        }
    }
}

/// Een knoop in de accessibility-boom.
#[derive(Clone, Debug)]
pub struct AccNode {
    pub id: u32,
    pub role: Role,
    pub name: String,
    /// De waarde (bv. de inhoud van een tekstveld); leeg indien n.v.t.
    pub value: String,
    /// Voor selectievakjes: aan/uit.
    pub checked: Option<bool>,
    pub children: Vec<AccNode>,
}

impl AccNode {
    pub fn new(id: u32, role: Role, name: &str) -> AccNode {
        AccNode { id, role, name: name.to_string(), value: String::new(), checked: None, children: Vec::new() }
    }
    pub fn with_value(mut self, v: &str) -> Self {
        self.value = v.to_string();
        self
    }
    pub fn checked(mut self, c: bool) -> Self {
        self.checked = Some(c);
        self
    }
    pub fn child(mut self, n: AccNode) -> Self {
        self.children.push(n);
        self
    }

    /// De schermlezer-aankondiging van déze knoop in `lang`, bv.
    /// `"knop: Opslaan"`, `"tekstveld: Naam, leeg"`, `"selectievakje: Akkoord, aangevinkt"`.
    pub fn announce(&self, lang: Lang) -> String {
        let mut s = String::from(self.role.label(lang));
        s.push_str(": ");
        s.push_str(&self.name);
        match self.role {
            Role::TextField => {
                s.push_str(", ");
                s.push_str(if self.value.is_empty() { empty_word(lang) } else { &self.value });
            }
            Role::CheckBox => {
                s.push_str(", ");
                s.push_str(checked_word(lang, self.checked.unwrap_or(false)));
            }
            _ => {}
        }
        s
    }
}

fn empty_word(lang: Lang) -> &'static str {
    match lang {
        Lang::Nl => "leeg",
        Lang::De => "leer",
        Lang::Fr => "vide",
        _ => "empty",
    }
}

fn checked_word(lang: Lang, on: bool) -> &'static str {
    match (lang, on) {
        (Lang::Nl, true) => "aangevinkt", (Lang::Nl, false) => "niet aangevinkt",
        (Lang::De, true) => "aktiviert", (Lang::De, false) => "deaktiviert",
        (Lang::Fr, true) => "coché", (Lang::Fr, false) => "non coché",
        (_, true) => "checked", (_, false) => "unchecked",
    }
}

/// Een accessibility-boom met focusbeheer.
pub struct AccTree {
    pub root: AccNode,
    /// De id van de huidig gefocuste knoop (0 = geen).
    pub focused: u32,
}

impl AccTree {
    pub fn new(root: AccNode) -> AccTree {
        AccTree { root, focused: 0 }
    }

    /// De focusbare knopen in leesvolgorde (diepte-eerst).
    pub fn focus_order(&self) -> Vec<u32> {
        let mut out = Vec::new();
        collect_focusable(&self.root, &mut out);
        out
    }

    /// Verplaats de focus naar de volgende (of vorige) focusbare knoop, cyclisch.
    /// Geeft de nieuwe gefocuste id terug.
    pub fn move_focus(&mut self, forward: bool) -> u32 {
        let order = self.focus_order();
        if order.is_empty() {
            return 0;
        }
        let cur = order.iter().position(|id| *id == self.focused);
        let next = match cur {
            Some(i) if forward => (i + 1) % order.len(),
            Some(i) => (i + order.len() - 1) % order.len(),
            None => 0, // nog geen focus → eerste
        };
        self.focused = order[next];
        self.focused
    }

    /// Zoek een knoop op id.
    pub fn find(&self, id: u32) -> Option<&AccNode> {
        find_node(&self.root, id)
    }

    /// De schermlezer-aankondiging van de gefocuste knoop in `lang`.
    pub fn announce_focused(&self, lang: Lang) -> Option<String> {
        self.find(self.focused).map(|n| n.announce(lang))
    }
}

fn collect_focusable<'a>(n: &'a AccNode, out: &mut Vec<u32>) {
    if n.role.focusable() {
        out.push(n.id);
    }
    for c in &n.children {
        collect_focusable(c, out);
    }
}

fn find_node(n: &AccNode, id: u32) -> Option<&AccNode> {
    if n.id == id {
        return Some(n);
    }
    for c in &n.children {
        if let Some(f) = find_node(c, id) {
            return Some(f);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dialog() -> AccTree {
        // venster: kop, tekstveld(Naam), selectievakje(Akkoord), knop(Opslaan), knop(Annuleren)
        let root = AccNode::new(1, Role::Window, "Aanmelden")
            .child(AccNode::new(2, Role::Heading, "Welkom"))
            .child(AccNode::new(3, Role::TextField, "Naam"))
            .child(AccNode::new(4, Role::CheckBox, "Akkoord").checked(false))
            .child(AccNode::new(5, Role::Button, "Opslaan"))
            .child(AccNode::new(6, Role::Button, "Annuleren"));
        AccTree::new(root)
    }

    #[test]
    fn only_focusable_in_order() {
        let t = dialog();
        // De kop (2) is niet focusbaar; tekstveld/checkbox/knoppen wel.
        assert_eq!(t.focus_order(), alloc::vec![3, 4, 5, 6]);
    }

    #[test]
    fn focus_navigation_cycles() {
        let mut t = dialog();
        assert_eq!(t.move_focus(true), 3); // eerste
        assert_eq!(t.move_focus(true), 4);
        assert_eq!(t.move_focus(true), 5);
        assert_eq!(t.move_focus(true), 6);
        assert_eq!(t.move_focus(true), 3); // cyclisch terug
        assert_eq!(t.move_focus(false), 6); // achteruit
    }

    #[test]
    fn screen_reader_announcements_nl() {
        let t = dialog();
        assert_eq!(t.find(5).unwrap().announce(Lang::Nl), "knop: Opslaan");
        assert_eq!(t.find(3).unwrap().announce(Lang::Nl), "tekstveld: Naam, leeg");
        assert_eq!(t.find(4).unwrap().announce(Lang::Nl), "selectievakje: Akkoord, niet aangevinkt");
    }

    #[test]
    fn announcements_localised() {
        let t = dialog();
        assert_eq!(t.find(5).unwrap().announce(Lang::De), "Schaltfläche: Opslaan");
        assert_eq!(t.find(5).unwrap().announce(Lang::Fr), "bouton: Opslaan");
        assert_eq!(t.find(5).unwrap().announce(Lang::En), "button: Opslaan");
    }

    #[test]
    fn announce_focused_follows_focus() {
        let mut t = dialog();
        t.move_focus(true); // focus → tekstveld(3)
        assert_eq!(t.announce_focused(Lang::Nl), Some("tekstveld: Naam, leeg".to_string()));
        t.move_focus(true); // → checkbox(4)
        assert_eq!(t.announce_focused(Lang::Nl), Some("selectievakje: Akkoord, niet aangevinkt".to_string()));
    }

    #[test]
    fn textfield_with_value() {
        let n = AccNode::new(9, Role::TextField, "E-mail").with_value("anke@euro-os.eu");
        assert_eq!(n.announce(Lang::Nl), "tekstveld: E-mail, anke@euro-os.eu");
    }
}
