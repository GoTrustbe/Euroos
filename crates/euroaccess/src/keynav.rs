//! 3F-3 — complete **keyboard navigation** for the accessibility tree.
//!
//! EN 301 549 / WCAG 2.1.1 require that everything be operable from the keyboard
//! alone. This is the pure state machine: given a key and the tree, it moves
//! focus, adjusts a slider, activates a control, or cancels a dialog, and returns
//! the resulting screen-reader announcement. The kernel feeds it real key events.

use crate::{AccTree, Action};
use alloc::string::String;
use eurolocale::Lang;

/// A navigation key (surfaced from the raw keyboard: Tab, arrows, Enter, …).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Tab,
    ShiftTab,
    Up,
    Down,
    Left,
    Right,
    Enter,
    Space,
    Escape,
    Home,
    End,
}

/// The outcome of handling one key: the new focus, any action performed, and the
/// announcement the screen reader should speak.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NavOutcome {
    pub focused: u32,
    pub action: Option<Action>,
    pub speech: Option<String>,
}

/// Handle one navigation key against `tree`, announcing in `lang`.
pub fn handle(tree: &mut AccTree, key: Key, lang: Lang) -> NavOutcome {
    let action = match key {
        Key::Tab | Key::Down => {
            tree.move_focus(true);
            None
        }
        Key::ShiftTab | Key::Up => {
            tree.move_focus(false);
            None
        }
        // Right/Left adjust a focused slider; otherwise they move focus.
        Key::Right => tree.adjust_focused(1).or_else(|| {
            tree.move_focus(true);
            None
        }),
        Key::Left => tree.adjust_focused(-1).or_else(|| {
            tree.move_focus(false);
            None
        }),
        Key::Enter | Key::Space => tree.activate_focused(),
        Key::Escape => Some(Action::Cancel),
        Key::Home => {
            if let Some(&f) = tree.focus_order().first() {
                tree.focused = f;
            }
            None
        }
        Key::End => {
            if let Some(&l) = tree.focus_order().last() {
                tree.focused = l;
            }
            None
        }
    };
    NavOutcome { focused: tree.focused, action, speech: tree.announce_focused(lang) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AccNode, AccTree, Role};

    fn tree() -> AccTree {
        let root = AccNode::new(1, Role::Dialog, "Instellingen")
            .child(AccNode::new(2, Role::CheckBox, "Hoog contrast").checked(false))
            .child(AccNode::new(3, Role::Slider, "Vergroting").range(1, 8, 2))
            .child(AccNode::new(4, Role::Button, "Sluiten"))
            .child(AccNode::new(5, Role::Button, "Verwijderen").disabled(true));
        AccTree::new(root)
    }

    #[test]
    fn tab_moves_and_announces() {
        let mut t = tree();
        let o = handle(&mut t, Key::Tab, Lang::Nl);
        assert_eq!(o.focused, 2);
        assert_eq!(o.speech.as_deref(), Some("selectievakje: Hoog contrast, niet aangevinkt"));
    }

    #[test]
    fn enter_toggles_checkbox() {
        let mut t = tree();
        handle(&mut t, Key::Tab, Lang::Nl); // focus checkbox
        let o = handle(&mut t, Key::Enter, Lang::Nl);
        assert_eq!(o.action, Some(Action::Toggle));
        assert_eq!(o.speech.as_deref(), Some("selectievakje: Hoog contrast, aangevinkt"));
    }

    #[test]
    fn arrows_adjust_slider() {
        let mut t = tree();
        handle(&mut t, Key::Tab, Lang::Nl); // checkbox
        handle(&mut t, Key::Tab, Lang::Nl); // slider (id 3)
        let o = handle(&mut t, Key::Right, Lang::Nl);
        assert_eq!(o.action, Some(Action::Increment));
        // 1..8, was 2 → 3 → ~28%.
        assert_eq!(t.find(3).unwrap().range, Some((1, 8, 3)));
    }

    #[test]
    fn disabled_button_does_not_activate() {
        let mut t = tree();
        t.focused = 5; // the disabled "Verwijderen" button
        let o = handle(&mut t, Key::Enter, Lang::Nl);
        assert_eq!(o.action, None);
        assert!(t.find(5).unwrap().announce(Lang::Nl).contains("uitgeschakeld"));
    }

    #[test]
    fn escape_cancels() {
        let mut t = tree();
        assert_eq!(handle(&mut t, Key::Escape, Lang::Nl).action, Some(Action::Cancel));
    }
}
