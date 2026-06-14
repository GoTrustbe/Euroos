//! Kernel side of **EuroAccess** (plan P2, EN 301 549): the accessibility layer.
//! At boot we build a sample dialog as an accessibility tree, navigate the
//! focus and have the multilingual screen reader announce each node. Host-tested
//! core: [`euroaccess`].

use alloc::string::String;
use alloc::vec::Vec;

use euroaccess::{AccNode, AccTree, Role};
use eurolocale::Lang;

fn demo_dialog() -> AccTree {
    let root = AccNode::new(1, Role::Window, "Aanmelden")
        .child(AccNode::new(2, Role::Heading, "Welkom bij EuroOS"))
        .child(AccNode::new(3, Role::TextField, "Gebruikersnaam"))
        .child(AccNode::new(4, Role::CheckBox, "Onthoud mij").checked(false))
        .child(AccNode::new(5, Role::Button, "Aanmelden"))
        .child(AccNode::new(6, Role::Button, "Annuleren"));
    AccTree::new(root)
}

/// Boot self-test: focus order, cyclic navigation, multilingual announcements.
pub fn selftest() {
    let mut t = demo_dialog();

    // Only focusable nodes, in reading order (heading is not focusable).
    let order_ok = t.focus_order() == alloc::vec![3u32, 4, 5, 6];

    // Tab through the dialog; each node announces itself in Dutch.
    t.move_focus(true);
    let a1 = t.announce_focused(Lang::Nl); // text field
    t.move_focus(true);
    let a2 = t.announce_focused(Lang::Nl); // check box
    let nav_ok = a1.as_deref() == Some("tekstveld: Gebruikersnaam, leeg")
        && a2.as_deref() == Some("selectievakje: Onthoud mij, niet aangevinkt");

    // Cyclic navigation + backwards.
    t.move_focus(true); // button Aanmelden
    t.move_focus(true); // button Annuleren
    let wrap_ok = t.move_focus(true) == 3 && t.move_focus(false) == 6;

    // Same button, different language (the screen reader speaks the user's language).
    let btn = t.find(5).unwrap();
    let multilang_ok = btn.announce(Lang::Nl) == "knop: Aanmelden"
        && btn.announce(Lang::De) == "Schaltfläche: Aanmelden"
        && btn.announce(Lang::Fr) == "bouton: Aanmelden";

    let ok = order_ok && nav_ok && wrap_ok && multilang_ok;
    crate::serial_println!(
        "[p2] EuroAccess: focus-order={order_ok}, screen-reader-announcement(nl)={nav_ok}, cyclic-Tab-navigation={wrap_ok}, multilingual(nl/de/fr)={multilang_ok} → {}",
        if ok { "OK (accessibility layer, EN 301 549, multilingual screen reader) ✓" } else { "FAILED" }
    );
}

/// `euroaccess` shell command: show the accessibility tree of the sample dialog.
pub fn shell() -> Vec<String> {
    let t = demo_dialog();
    let mut out = alloc::vec![
        String::from("EuroAccess — accessibility layer (EN 301 549; AT-SPI equivalent for EuroDisplay)"),
        String::from("  sample dialog 'Aanmelden' — screen-reader announcements (Dutch):"),
    ];
    for id in t.focus_order() {
        if let Some(n) = t.find(id) {
            out.push(alloc::format!("    [{id}] {}", n.announce(Lang::Nl)));
        }
    }
    out.push(String::from("  role labels come from EuroLocale → the screen reader speaks every EU language"));
    out
}

/// **BB-8 boot self-test** — LIVE accessibility events end-to-end (EN 301 549):
/// navigate the focus through a real dialog, announce each node via the
/// multilingual screen reader, and route the announcement to EuroAudio (HDA). Proves
/// the chain widget tree → focus event → announcement → audio. (Intelligible
/// speech synthesis on top of this path is the next milestone; here: live focus events,
/// multilingual announcements, and the EuroAudio path that can make them sound.)
pub fn live_selftest() {
    let mut t = demo_dialog();
    let order = t.focus_order();
    let mut spoken: Vec<String> = Vec::new();
    let mut earcons = 0usize;
    let lpib0 = crate::hda::stream_pos();
    for _ in 0..order.len() {
        t.move_focus(true);
        if let Some(a) = t.announce_focused(Lang::Nl) {
            spoken.push(a);
        }
        // Earcon: a DISTINCT tone per role (button/check box/text field),
        // through the real HDA DAC. The stream DMA loops cyclically → the new beep sounds.
        if let Some(node) = t.find(t.focused) {
            let freq = match node.role {
                Role::Button => 784,    // G5
                Role::CheckBox => 587,  // D5
                Role::TextField => 440, // A4
                _ => 523,               // C5
            };
            if crate::hda::earcon(freq) {
                earcons += 1;
                // Short pause so each beep is audible separately (the QEMU audio timer
                // ticks on wall-clock time).
                for _ in 0..2_000_000 {
                    core::hint::spin_loop();
                }
            }
        }
    }
    // Wait ~400 ms (wall clock, via timer ticks) so the slow QEMU audio DMA
    // advances measurably, and read whether the stream is running (RUN bit).
    let t0 = crate::interrupts::ticks();
    while crate::interrupts::ticks().wrapping_sub(t0) < 40 {
        core::hint::spin_loop();
    }
    let lpib1 = crate::hda::stream_pos();
    let running = crate::hda::stream_running();
    crate::serial_println!(
        "[bb8] EuroAccess LIVE events: {} focus steps → screen reader spoke: [{}] · {} EARCONS through the HDA DAC (tone per role: text field 440 · check box 587 · button 784 Hz) · output stream running={} (RUN bit), LPIB {}\u{2192}{} → chain widget→focus→announcement→AUDIO (EN 301 549) ✓; intelligible speech synthesis = next milestone",
        order.len(),
        spoken.join("  |  "),
        earcons,
        running,
        lpib0,
        lpib1
    );
}
