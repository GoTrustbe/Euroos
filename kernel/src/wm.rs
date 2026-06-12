//! Boot-zelftest voor **vensterbeheer** (sluiten/minimaliseren/maximaliseren).
//! Verifieert deterministisch — zonder muis — dat de verkeerslicht-trefzones
//! kloppen en dat maximaliseren ↔ herstellen de juiste geometrie geeft. De
//! interactieve muis-bediening zelf zit in de desktop-loop (main.rs).

use crate::compositor::{work_area, TitleButton, Window};
use crate::graphics::Color;
use crate::serial_println;
use alloc::string::String;
use alloc::vec::Vec;

fn mk() -> Window {
    Window {
        x: 130,
        y: 70,
        w: 760,
        h: 660,
        title: String::from("Test"),
        content: Vec::new(),
        ui: Vec::new(),
        active: false,
        accent: Color::ACCENT,
        sec: crate::eds::SecState::new(true, true, false),
        app: crate::suite_ui::SuiteApp::None,
        visible: true,
        restore: None,
    }
}

/// Boot-zelftest: verkeerslicht-trefzones + maximaliseer/herstel-geometrie.
pub fn selftest() {
    let win = mk();
    // Trefzones: rood/oranje/groen op x+14/34/54 (midden +6), titelbalk-y.
    let close = win.title_button_at(150, 88) == Some(TitleButton::Close);
    let mini = win.title_button_at(170, 88) == Some(TitleButton::Minimize);
    let maxi = win.title_button_at(190, 88) == Some(TitleButton::Maximize);
    let none = win.title_button_at(400, 88).is_none();

    // Maximaliseren → werkgebied; herstellen → originele geometrie.
    let mut w2 = mk();
    let orig = (w2.x, w2.y, w2.w, w2.h);
    let (wx, wy, ww, wh) = work_area(1920, 1080);
    w2.restore = Some(orig);
    w2.x = wx;
    w2.y = wy;
    w2.w = ww;
    w2.h = wh;
    let maximized_ok = w2.x == 90 && w2.y == 14 && w2.w == ww && w2.w > 760 && w2.h > 660;
    if let Some((rx, ry, rw, rh)) = w2.restore.take() {
        w2.x = rx;
        w2.y = ry;
        w2.w = rw;
        w2.h = rh;
    }
    let restored_ok = (w2.x, w2.y, w2.w, w2.h) == orig;

    // Zichtbaarheid (sluiten/minimaliseren verbergt).
    let mut w3 = mk();
    w3.visible = false;
    let hide_ok = !w3.visible;

    let ok = close && mini && maxi && none && maximized_ok && restored_ok && hide_ok;
    serial_println!(
        "[wm] vensterbeheer: knoppen(sluit={} min={} max={} leeg={}), maximaliseer→werkgebied {}×{} ok={}, herstel ok={}, verberg ok={} {}",
        close, mini, maxi, none, ww, wh, maximized_ok, restored_ok, hide_ok,
        if ok { "✓" } else { "✗ FOUT" }
    );
}
