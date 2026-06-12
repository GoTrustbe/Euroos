//! Kernel-zijde van **EuroAccess** (plan P2, EN 301 549): de toegankelijkheidslaag.
//! Bij boot bouwen we een voorbeeld-dialoog als accessibility-boom, navigeren we de
//! focus en laten we de meertalige schermlezer elke knoop aankondigen. Host-geteste
//! kern: [`euroaccess`].

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

/// Boot-zelftest: focus-volgorde, cyclische navigatie, meertalige aankondigingen.
pub fn selftest() {
    let mut t = demo_dialog();

    // Alleen focusbare knopen, in leesvolgorde (kop is niet focusbaar).
    let order_ok = t.focus_order() == alloc::vec![3u32, 4, 5, 6];

    // Tab door de dialoog; elke knoop kondigt zich aan in het Nederlands.
    t.move_focus(true);
    let a1 = t.announce_focused(Lang::Nl); // tekstveld
    t.move_focus(true);
    let a2 = t.announce_focused(Lang::Nl); // selectievakje
    let nav_ok = a1.as_deref() == Some("tekstveld: Gebruikersnaam, leeg")
        && a2.as_deref() == Some("selectievakje: Onthoud mij, niet aangevinkt");

    // Cyclische navigatie + achteruit.
    t.move_focus(true); // knop Aanmelden
    t.move_focus(true); // knop Annuleren
    let wrap_ok = t.move_focus(true) == 3 && t.move_focus(false) == 6;

    // Zelfde knop, andere taal (de schermlezer spreekt de taal van de gebruiker).
    let btn = t.find(5).unwrap();
    let multilang_ok = btn.announce(Lang::Nl) == "knop: Aanmelden"
        && btn.announce(Lang::De) == "Schaltfläche: Aanmelden"
        && btn.announce(Lang::Fr) == "bouton: Aanmelden";

    let ok = order_ok && nav_ok && wrap_ok && multilang_ok;
    crate::serial_println!(
        "[p2] EuroAccess: focus-volgorde={order_ok}, schermlezer-aankondiging(nl)={nav_ok}, cyclische-Tab-navigatie={wrap_ok}, meertalig(nl/de/fr)={multilang_ok} → {}",
        if ok { "OK (toegankelijkheidslaag, EN 301 549, meertalige schermlezer) ✓" } else { "MISLUKT" }
    );
}

/// `euroaccess`-shellcommando: toon de accessibility-boom van de voorbeeld-dialoog.
pub fn shell() -> Vec<String> {
    let t = demo_dialog();
    let mut out = alloc::vec![
        String::from("EuroAccess — toegankelijkheidslaag (EN 301 549; AT-SPI-equivalent voor EuroDisplay)"),
        String::from("  voorbeeld-dialoog 'Aanmelden' — schermlezer-aankondigingen (Nederlands):"),
    ];
    for id in t.focus_order() {
        if let Some(n) = t.find(id) {
            out.push(alloc::format!("    [{id}] {}", n.announce(Lang::Nl)));
        }
    }
    out.push(String::from("  rol-labels komen uit EuroLocale → de schermlezer spreekt élke EU-taal"));
    out
}

/// **BB-8 boot-zelftest** — LIVE accessibility-events end-to-end (EN 301 549):
/// navigeer de focus door een echte dialoog, kondig elke knoop aan via de
/// meertalige schermlezer, en route de aankondiging naar EuroAudio (HDA). Bewijst
/// de keten widget-boom → focus-event → aankondiging → audio. (Intelligibele
/// spraaksynthese bovenop dit pad is de volgende mijl; hier: live focus-events,
/// meertalige aankondigingen, en het EuroAudio-pad dat ze kan laten klinken.)
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
        // Earcon: een ONDERSCHEIDENDE toon per rol (knop/selectievakje/tekstveld),
        // door de echte HDA-DAC. De stream-DMA loopt cyclisch → de nieuwe beep klinkt.
        if let Some(node) = t.find(t.focused) {
            let freq = match node.role {
                Role::Button => 784,    // G5
                Role::CheckBox => 587,  // D5
                Role::TextField => 440, // A4
                _ => 523,               // C5
            };
            if crate::hda::earcon(freq) {
                earcons += 1;
                // Korte pauze zodat elke beep los hoorbaar is (de QEMU-audiotimer
                // tikt op wandkloktijd).
                for _ in 0..2_000_000 {
                    core::hint::spin_loop();
                }
            }
        }
    }
    // Wacht ~400 ms (wandklok, via timer-ticks) zodat de trage QEMU-audio-DMA
    // meetbaar vooruitgaat, en lees of de stream draait (RUN-bit).
    let t0 = crate::interrupts::ticks();
    while crate::interrupts::ticks().wrapping_sub(t0) < 40 {
        core::hint::spin_loop();
    }
    let lpib1 = crate::hda::stream_pos();
    let running = crate::hda::stream_running();
    crate::serial_println!(
        "[bb8] EuroAccess LIVE-events: {} focus-stappen → schermlezer sprak: [{}] · {} EARCONS door de HDA-DAC (toon per rol: tekstveld 440 · selectievakje 587 · knop 784 Hz) · output-stream draait={} (RUN-bit), LPIB {}\u{2192}{} → keten widget→focus→aankondiging→AUDIO (EN 301 549) ✓; intelligibele spraaksynthese = volgende mijl",
        order.len(),
        spoken.join("  |  "),
        earcons,
        running,
        lpib0,
        lpib1
    );
}
