//! EuroAccess — the accessibility layer of EuroDisplay (plan P2).
//!
//! Accessibility is a *procurement requirement* in the EU (EN 301 549), not an extra.
//! This crate is the AT-SPI equivalent: an **accessibility tree** (roles, names,
//! states), **focus management** (next/previous focusable node in reading order) and
//! a **multilingual screen reader** that announces each node in the user's
//! language — sovereign and accessible, because the role labels come from EuroLocale.
//!
//! Pure, host-tested `no_std` logic; EuroDisplay fills the tree with real widgets.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec::Vec;
use eurolocale::Lang;

pub mod keynav;
pub mod magnify;
pub mod theme;

/// An 8-bit-per-channel sRGB colour (shared by the theme/contrast layer).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

/// The on-screen bounding box of an accessibility node (drives follow-focus
/// magnification and the focus ring).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}
impl Rect {
    pub fn new(x: i32, y: i32, w: u32, h: u32) -> Rect {
        Rect { x, y, w, h }
    }
    pub fn center(&self) -> (i32, i32) {
        (self.x + self.w as i32 / 2, self.y + self.h as i32 / 2)
    }
}

/// An action a user can invoke on a focused node (keyboard or AT).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Activate,  // press a button / open a link / menu item
    Toggle,    // flip a checkbox
    Select,    // choose a radio / list item
    Increment, // slider up
    Decrement, // slider down
    Cancel,    // Escape — dismiss a dialog
}

/// The role of a UI element (a subset of the ARIA/AT-SPI roles).
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
    Slider,
    Radio,
    Tab,
    ProgressBar,
    Dialog,
    Panel,
    Toolbar,
}

impl Role {
    /// Is an element with this role focusable by default (keyboard navigation)?
    fn focusable(self) -> bool {
        matches!(
            self,
            Role::Button
                | Role::TextField
                | Role::CheckBox
                | Role::ListItem
                | Role::MenuItem
                | Role::Link
                | Role::Slider
                | Role::Radio
                | Role::Tab
        )
    }

    /// The role label in the user's language (the screen reader speaks EU languages).
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
            // New roles (3F-3), nl/de/fr.
            (Nl, Slider) => "schuifregelaar", (Nl, Radio) => "keuzerondje", (Nl, Tab) => "tabblad",
            (Nl, ProgressBar) => "voortgangsbalk", (Nl, Dialog) => "dialoogvenster", (Nl, Panel) => "paneel",
            (Nl, Toolbar) => "werkbalk",
            (De, Slider) => "Schieberegler", (De, Radio) => "Optionsfeld", (De, Tab) => "Registerkarte",
            (De, ProgressBar) => "Fortschrittsbalken", (De, Dialog) => "Dialogfeld", (De, Panel) => "Bereich",
            (De, Toolbar) => "Symbolleiste",
            (Fr, Slider) => "curseur", (Fr, Radio) => "bouton radio", (Fr, Tab) => "onglet",
            (Fr, ProgressBar) => "barre de progression", (Fr, Dialog) => "boîte de dialogue", (Fr, Panel) => "panneau",
            (Fr, Toolbar) => "barre d'outils",
            // English fallback for all other languages.
            (_, Window) => "window", (_, Heading) => "heading", (_, Label) => "label",
            (_, Button) => "button", (_, TextField) => "text field", (_, CheckBox) => "checkbox",
            (_, List) => "list", (_, ListItem) => "list item", (_, Menu) => "menu",
            (_, MenuItem) => "menu item", (_, Link) => "link",
            (_, Slider) => "slider", (_, Radio) => "radio button", (_, Tab) => "tab",
            (_, ProgressBar) => "progress bar", (_, Dialog) => "dialog", (_, Panel) => "panel",
            (_, Toolbar) => "toolbar",
        }
    }
}

/// A node in the accessibility tree.
#[derive(Clone, Debug)]
pub struct AccNode {
    pub id: u32,
    pub role: Role,
    pub name: String,
    /// The value (e.g. the contents of a text field); empty if not applicable.
    pub value: String,
    /// For check boxes: on/off.
    pub checked: Option<bool>,
    /// Greyed-out / not interactive.
    pub disabled: bool,
    /// For radios / list items / tabs: selected or not.
    pub selected: Option<bool>,
    /// For menus / tree items: expanded or collapsed.
    pub expanded: Option<bool>,
    /// For sliders / progress bars: (min, max, current).
    pub range: Option<(i32, i32, i32)>,
    /// On-screen bounds (for the focus ring + follow-focus magnifier).
    pub bounds: Rect,
    pub children: Vec<AccNode>,
}

impl AccNode {
    pub fn new(id: u32, role: Role, name: &str) -> AccNode {
        AccNode {
            id,
            role,
            name: name.to_string(),
            value: String::new(),
            checked: None,
            disabled: false,
            selected: None,
            expanded: None,
            range: None,
            bounds: Rect::default(),
            children: Vec::new(),
        }
    }
    pub fn with_value(mut self, v: &str) -> Self {
        self.value = v.to_string();
        self
    }
    pub fn checked(mut self, c: bool) -> Self {
        self.checked = Some(c);
        self
    }
    pub fn disabled(mut self, d: bool) -> Self {
        self.disabled = d;
        self
    }
    pub fn selected(mut self, s: bool) -> Self {
        self.selected = Some(s);
        self
    }
    pub fn expanded(mut self, e: bool) -> Self {
        self.expanded = Some(e);
        self
    }
    /// A slider/progress value: min..=max, current.
    pub fn range(mut self, min: i32, max: i32, val: i32) -> Self {
        self.range = Some((min, max, val.clamp(min, max)));
        self
    }
    pub fn at(mut self, x: i32, y: i32, w: u32, h: u32) -> Self {
        self.bounds = Rect::new(x, y, w, h);
        self
    }
    pub fn child(mut self, n: AccNode) -> Self {
        self.children.push(n);
        self
    }

    /// The value of a range node as a percentage (0–100), for announcements.
    pub fn percent(&self) -> Option<u32> {
        self.range.map(|(lo, hi, v)| {
            if hi <= lo {
                0
            } else {
                (((v - lo) as i64 * 100) / (hi - lo) as i64) as u32
            }
        })
    }

    /// The screen-reader announcement of *this* node in `lang`, e.g.
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
            Role::Slider | Role::ProgressBar => {
                if let Some(p) = self.percent() {
                    s.push_str(", ");
                    s.push_str(&alloc::format!("{p}%"));
                }
            }
            _ => {}
        }
        // Selection (radios / tabs / list items).
        if let Some(sel) = self.selected {
            s.push_str(", ");
            s.push_str(selected_word(lang, sel));
        }
        // Expanded / collapsed (menus / disclosure).
        if let Some(ex) = self.expanded {
            s.push_str(", ");
            s.push_str(expanded_word(lang, ex));
        }
        // Disabled state is announced last (EN 301 549: convey state, not only look).
        if self.disabled {
            s.push_str(", ");
            s.push_str(disabled_word(lang));
        }
        s
    }
}

fn selected_word(lang: Lang, on: bool) -> &'static str {
    match (lang, on) {
        (Lang::Nl, true) => "geselecteerd", (Lang::Nl, false) => "niet geselecteerd",
        (Lang::De, true) => "ausgewählt", (Lang::De, false) => "nicht ausgewählt",
        (Lang::Fr, true) => "sélectionné", (Lang::Fr, false) => "non sélectionné",
        (_, true) => "selected", (_, false) => "not selected",
    }
}
fn expanded_word(lang: Lang, on: bool) -> &'static str {
    match (lang, on) {
        (Lang::Nl, true) => "uitgevouwen", (Lang::Nl, false) => "samengevouwen",
        (Lang::De, true) => "erweitert", (Lang::De, false) => "reduziert",
        (Lang::Fr, true) => "développé", (Lang::Fr, false) => "réduit",
        (_, true) => "expanded", (_, false) => "collapsed",
    }
}
fn disabled_word(lang: Lang) -> &'static str {
    match lang {
        Lang::Nl => "uitgeschakeld",
        Lang::De => "deaktiviert",
        Lang::Fr => "désactivé",
        _ => "disabled",
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

/// An accessibility tree with focus management.
pub struct AccTree {
    pub root: AccNode,
    /// The id of the currently focused node (0 = none).
    pub focused: u32,
}

impl AccTree {
    pub fn new(root: AccNode) -> AccTree {
        AccTree { root, focused: 0 }
    }

    /// The focusable nodes in reading order (depth-first).
    pub fn focus_order(&self) -> Vec<u32> {
        let mut out = Vec::new();
        collect_focusable(&self.root, &mut out);
        out
    }

    /// Move the focus to the next (or previous) focusable node, cyclically.
    /// Returns the new focused id.
    pub fn move_focus(&mut self, forward: bool) -> u32 {
        let order = self.focus_order();
        if order.is_empty() {
            return 0;
        }
        let cur = order.iter().position(|id| *id == self.focused);
        let next = match cur {
            Some(i) if forward => (i + 1) % order.len(),
            Some(i) => (i + order.len() - 1) % order.len(),
            None => 0, // no focus yet → first
        };
        self.focused = order[next];
        self.focused
    }

    /// Look up a node by id.
    pub fn find(&self, id: u32) -> Option<&AccNode> {
        find_node(&self.root, id)
    }

    /// The screen-reader announcement of the focused node in `lang`.
    pub fn announce_focused(&self, lang: Lang) -> Option<String> {
        self.find(self.focused).map(|n| n.announce(lang))
    }

    /// Look up a node by id (mutable).
    pub fn find_mut(&mut self, id: u32) -> Option<&mut AccNode> {
        find_node_mut(&mut self.root, id)
    }

    /// The on-screen bounds of the focused node (for the focus ring / magnifier).
    pub fn focused_bounds(&self) -> Option<Rect> {
        self.find(self.focused).map(|n| n.bounds)
    }

    /// Activate the focused node (Enter/Space): toggle a checkbox, select a
    /// radio/list item/tab, or activate a button/link/menu item. Returns the
    /// action performed (`None` if the node is disabled or not actionable).
    pub fn activate_focused(&mut self) -> Option<Action> {
        let id = self.focused;
        let n = self.find_mut(id)?;
        if n.disabled {
            return None;
        }
        match n.role {
            Role::CheckBox => {
                let now = !n.checked.unwrap_or(false);
                n.checked = Some(now);
                Some(Action::Toggle)
            }
            Role::Radio | Role::ListItem | Role::Tab => {
                n.selected = Some(true);
                Some(Action::Select)
            }
            Role::Button | Role::Link | Role::MenuItem => Some(Action::Activate),
            _ => None,
        }
    }

    /// Adjust the focused slider by `delta` (arrow keys). Returns the resulting
    /// action, or `None` if the focused node is not an adjustable range.
    pub fn adjust_focused(&mut self, delta: i32) -> Option<Action> {
        let id = self.focused;
        let n = self.find_mut(id)?;
        if n.disabled || delta == 0 {
            return None;
        }
        match n.role {
            Role::Slider => {
                let (lo, hi, v) = n.range?;
                n.range = Some((lo, hi, (v + delta).clamp(lo, hi)));
                Some(if delta > 0 { Action::Increment } else { Action::Decrement })
            }
            _ => None,
        }
    }
}

fn collect_focusable(n: &AccNode, out: &mut Vec<u32>) {
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

fn find_node_mut(n: &mut AccNode, id: u32) -> Option<&mut AccNode> {
    if n.id == id {
        return Some(n);
    }
    for c in &mut n.children {
        if let Some(f) = find_node_mut(c, id) {
            return Some(f);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dialog() -> AccTree {
        // window: heading, text field(Naam), check box(Akkoord), button(Opslaan), button(Annuleren)
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
        // The heading (2) is not focusable; text field/checkbox/buttons are.
        assert_eq!(t.focus_order(), alloc::vec![3, 4, 5, 6]);
    }

    #[test]
    fn focus_navigation_cycles() {
        let mut t = dialog();
        assert_eq!(t.move_focus(true), 3); // first
        assert_eq!(t.move_focus(true), 4);
        assert_eq!(t.move_focus(true), 5);
        assert_eq!(t.move_focus(true), 6);
        assert_eq!(t.move_focus(true), 3); // cycle back
        assert_eq!(t.move_focus(false), 6); // backwards
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
        t.move_focus(true); // focus → text field(3)
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
