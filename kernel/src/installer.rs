//! **EuroInstall** — the guided graphical installer (wizard).
//!
//! The live medium boots the desktop from RAM and never touches a disk on its
//! own. This window walks the user through language, keyboard, target disk,
//! account, encryption and a summary; only the final "Install" writes the disk,
//! through the proven executor in [`crate::instexec`] (GPT, FAT32 ESP with the
//! loader and the A/B kernel slots from the medium's own ESP, EuroFS root,
//! provisioning, TPM-sealed FDE). The account is a real EuroID record
//! (Argon2id, `wheel`), so the installed system's lockscreen accepts the
//! password typed here. Keyboard and mouse both drive the wizard.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use euroinstall::{plan, Config, Disk, Step};
use spin::Mutex;

use crate::graphics::{Color, FrameBuffer};
use crate::ps2::Key;
use crate::text;

const TITLEBAR_H: usize = 44;
const MIB: u64 = 1024 * 1024;

/// Languages offered by the wizard (tag → label). Every tag parses with
/// `eurolocale`, which the planner validates again.
const LANGS: &[(&str, &str)] = &[
    ("nl-BE", "Dutch (Belgium)"),
    ("nl-NL", "Dutch (Netherlands)"),
    ("en-GB", "English (United Kingdom)"),
    ("en-IE", "English (Ireland)"),
    ("fr-BE", "French (Belgium)"),
    ("fr-FR", "French (France)"),
    ("de-DE", "German (Germany)"),
    ("de-AT", "German (Austria)"),
    ("es-ES", "Spanish"),
    ("it-IT", "Italian"),
    ("pt-PT", "Portuguese"),
    ("pl-PL", "Polish"),
    ("sv-SE", "Swedish"),
    ("da-DK", "Danish"),
    ("fi-FI", "Finnish"),
    ("cs-CZ", "Czech"),
];

/// Keyboard layouts the kernel's keymap driver implements (tag → label).
const KEYMAPS: &[(&str, &str)] = &[
    ("us-qwerty", "US QWERTY"),
    ("be-azerty", "Belgian AZERTY"),
    ("fr-azerty", "French AZERTY"),
    ("de-qwertz", "German QWERTZ"),
];

const PAGE_NAMES: [&str; 8] = ["Welcome", "Language", "Keyboard", "Disk", "Account", "Encryption", "Summary", "Install"];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Welcome,
    Language,
    Keyboard,
    Disk,
    Account,
    Encryption,
    Summary,
    Installing,
    Done,
    Failed,
}

impl Page {
    fn index(self) -> usize {
        match self {
            Page::Welcome => 0,
            Page::Language => 1,
            Page::Keyboard => 2,
            Page::Disk => 3,
            Page::Account => 4,
            Page::Encryption => 5,
            Page::Summary => 6,
            Page::Installing | Page::Done | Page::Failed => 7,
        }
    }
}

/// A disk the wizard can see. Only `blank` disks are selectable: a disk with a
/// boot signature holds someone's data and is never offered for erasure.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Bus {
    Virtio(usize),
    Nvme,
    Ahci(usize),
}

#[derive(Clone)]
struct Target {
    bus: Bus,
    name: String,
    mib: u64,
    blank: bool,
}

impl Target {
    fn selectable(&self) -> bool {
        self.blank && self.mib * MIB >= euroinstall::minimum_disk_bytes()
    }
}

enum Action {
    Install,
    Reboot,
}

struct Wizard {
    page: Page,
    lang: usize,
    keymap: usize,
    targets: Vec<Target>,
    disk: usize,
    hostname: String,
    username: String,
    password: String,
    password2: String,
    /// Focused field on the Account page: 0 hostname, 1 username, 2 password, 3 repeat.
    field: usize,
    fde: bool,
    error: String,
    log: Vec<String>,
    installed_to: String,
}

static WIZARD: Mutex<Wizard> = Mutex::new(Wizard {
    page: Page::Welcome,
    lang: 0,
    keymap: 1,
    targets: Vec::new(),
    disk: 0,
    hostname: String::new(),
    username: String::new(),
    password: String::new(),
    password2: String::new(),
    field: 0,
    fde: true,
    error: String::new(),
    log: Vec::new(),
    installed_to: String::new(),
});

fn scan_targets() -> Vec<Target> {
    let mut out = Vec::new();
    let n = crate::virtio_blk::device_count();
    for d in 0..n {
        if !crate::virtio_blk::present_dev(d) {
            continue;
        }
        let mib = crate::virtio_blk::capacity_sectors_dev(d) * 512 / MIB;
        out.push(Target { bus: Bus::Virtio(d), name: alloc::format!("virtio disk {d}"), mib, blank: crate::instexec::disk_is_blank(d) });
    }
    if crate::nvme::present() {
        let mib = crate::nvme::capacity_sectors() * 512 / MIB;
        out.push(Target { bus: Bus::Nvme, name: String::from("NVMe disk"), mib, blank: crate::instexec::nvme_is_blank() });
    }
    for idx in 0..crate::ahci::disk_count() {
        let mib = crate::ahci::disk_sectors(idx) * 512 / MIB;
        out.push(Target { bus: Bus::Ahci(idx), name: alloc::format!("SATA disk {idx}"), mib, blank: crate::instexec::ahci_is_blank(idx) });
    }
    out
}

fn size_label(mib: u64) -> String {
    if mib >= 1024 {
        alloc::format!("{}.{} GiB", mib / 1024, (mib % 1024) * 10 / 1024)
    } else {
        alloc::format!("{mib} MiB")
    }
}

impl Wizard {
    fn config(&self) -> Config {
        let mib = self.targets.get(self.disk).map(|t| t.mib).unwrap_or(0);
        Config {
            disk: Disk { total_bytes: mib * MIB },
            locale: LANGS[self.lang].0.to_string(),
            keymap: KEYMAPS[self.keymap].0.to_string(),
            hostname: self.hostname.clone(),
            username: self.username.clone(),
            password: self.password.clone(),
            fde: self.fde && crate::tpm::present(),
            live: false,
        }
    }

    fn refresh_targets(&mut self) {
        self.targets = scan_targets();
        if self.disk >= self.targets.len() {
            self.disk = 0;
        }
        // Pre-select the first disk that may be installed to.
        if !self.targets.get(self.disk).map(|t| t.selectable()).unwrap_or(false) {
            if let Some(i) = self.targets.iter().position(|t| t.selectable()) {
                self.disk = i;
            }
        }
    }

    fn back(&mut self) {
        self.error.clear();
        self.page = match self.page {
            Page::Welcome | Page::Installing | Page::Done => self.page,
            Page::Language => Page::Welcome,
            Page::Keyboard => Page::Language,
            Page::Disk => Page::Keyboard,
            Page::Account => Page::Disk,
            Page::Encryption => Page::Account,
            Page::Summary => Page::Encryption,
            Page::Failed => Page::Summary,
        };
    }

    /// Validate the current page and advance. `Some(action)` when the kernel
    /// loop must do something that the wizard state cannot (install, reboot).
    fn next(&mut self) -> Option<Action> {
        self.error.clear();
        match self.page {
            Page::Welcome => {
                if !crate::instexec::media_available() {
                    self.error = String::from("No install media: boot this medium via UEFI with a target disk present.");
                    return None;
                }
                self.page = Page::Language;
            }
            Page::Language => self.page = Page::Keyboard,
            Page::Keyboard => {
                // Applied live: the account page is typed in the chosen layout.
                crate::ps2::set_layout_tag(KEYMAPS[self.keymap].0);
                self.refresh_targets();
                self.page = Page::Disk;
            }
            Page::Disk => {
                let Some(t) = self.targets.get(self.disk) else {
                    self.error = String::from("No target disk found. Attach a blank disk and go back.");
                    return None;
                };
                if !t.blank {
                    self.error = String::from("That disk is in use. Only a blank disk can be installed to.");
                    return None;
                }
                if !t.selectable() {
                    self.error = alloc::format!(
                        "That disk is too small: EuroOS needs at least {}.",
                        size_label(euroinstall::minimum_disk_bytes() / MIB)
                    );
                    return None;
                }
                self.page = Page::Account;
                self.field = 0;
            }
            Page::Account => {
                if self.hostname.trim().is_empty() {
                    self.error = String::from("Enter a computer name.");
                    self.field = 0;
                    return None;
                }
                if let Err(e) = crate::euroid::check_username(&self.username) {
                    self.error = e;
                    self.field = 1;
                    return None;
                }
                if let Err(e) = crate::euroid::check_password(&self.password) {
                    self.error = e;
                    self.field = 2;
                    return None;
                }
                if self.password != self.password2 {
                    self.error = String::from("The passwords do not match.");
                    self.field = 3;
                    return None;
                }
                self.page = Page::Encryption;
            }
            Page::Encryption => self.page = Page::Summary,
            Page::Summary => {
                if let Err(e) = euroinstall::validate(&self.config()) {
                    self.error = alloc::format!("Configuration rejected: {e:?}");
                    return None;
                }
                self.log.clear();
                self.page = Page::Installing;
                return Some(Action::Install);
            }
            Page::Installing => {}
            Page::Done => return Some(Action::Reboot),
            Page::Failed => self.page = Page::Summary,
        }
        None
    }

    fn list_len(&self) -> usize {
        match self.page {
            Page::Language => LANGS.len(),
            Page::Keyboard => KEYMAPS.len(),
            Page::Disk => self.targets.len(),
            _ => 0,
        }
    }

    fn sel_mut(&mut self) -> Option<&mut usize> {
        match self.page {
            Page::Language => Some(&mut self.lang),
            Page::Keyboard => Some(&mut self.keymap),
            Page::Disk => Some(&mut self.disk),
            _ => None,
        }
    }

    fn move_sel(&mut self, delta: isize) {
        let n = self.list_len();
        if n == 0 {
            return;
        }
        if let Some(s) = self.sel_mut() {
            let cur = *s as isize;
            *s = (cur + delta).rem_euclid(n as isize) as usize;
        }
    }

    fn field_mut(&mut self) -> &mut String {
        match self.field {
            0 => &mut self.hostname,
            1 => &mut self.username,
            2 => &mut self.password,
            _ => &mut self.password2,
        }
    }

    fn type_char(&mut self, c: char) {
        let field = self.field;
        let s = self.field_mut();
        match field {
            0 => {
                let c = c.to_ascii_lowercase();
                if (c.is_ascii_alphanumeric() || c == '-') && s.len() < 32 {
                    s.push(c);
                }
            }
            1 => {
                let c = c.to_ascii_lowercase();
                if (c.is_ascii_alphanumeric() || c == '-' || c == '_') && s.len() < 32 {
                    s.push(c);
                }
            }
            _ => {
                if !c.is_control() && s.len() < 128 {
                    s.push(c);
                }
            }
        }
    }

    fn handle_key(&mut self, k: Key) -> Option<Action> {
        match self.page {
            Page::Installing => None,
            Page::Done => match k {
                Key::Enter => Some(Action::Reboot),
                _ => None,
            },
            Page::Failed => {
                if matches!(k, Key::Enter | Key::Esc) {
                    self.page = Page::Summary;
                }
                None
            }
            Page::Account => match k {
                Key::Esc => {
                    self.back();
                    None
                }
                Key::Tab | Key::Down => {
                    self.field = (self.field + 1) % 4;
                    None
                }
                Key::Up => {
                    self.field = (self.field + 3) % 4;
                    None
                }
                Key::Enter => {
                    if self.field < 3 {
                        self.field += 1;
                        None
                    } else {
                        self.next()
                    }
                }
                Key::Backspace => {
                    self.field_mut().pop();
                    None
                }
                Key::Char(c) => {
                    self.type_char(c);
                    None
                }
                _ => None,
            },
            Page::Encryption => match k {
                Key::Esc => {
                    self.back();
                    None
                }
                Key::Enter => self.next(),
                Key::Char(' ') | Key::Left | Key::Right => {
                    if crate::tpm::present() {
                        self.fde = !self.fde;
                    }
                    None
                }
                _ => None,
            },
            _ => match k {
                Key::Esc => {
                    self.back();
                    None
                }
                Key::Enter => self.next(),
                Key::Up => {
                    self.move_sel(-1);
                    None
                }
                Key::Down => {
                    self.move_sel(1);
                    None
                }
                _ => None,
            },
        }
    }
}

/// Should the wizard be open when the desktop starts? Yes on a live boot of a
/// medium that carries install files; never on an installed system.
pub fn open_on_live_boot() -> bool {
    crate::instexec::media_available() && !crate::rootblk::booted_from_disk()
}

// ── Geometry shared by render, click and the progress painter ──────────────

struct Geom {
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    cx: usize,
    cw: usize,
    list_y: usize,
    row_h: usize,
    rows: usize,
    back: (usize, usize, usize, usize),
    next: (usize, usize, usize, usize),
}

fn geom(win_x: usize, win_y: usize, win_w: usize, win_h: usize) -> Geom {
    let x = win_x;
    let y = win_y + TITLEBAR_H;
    let w = win_w;
    let h = win_h.saturating_sub(TITLEBAR_H);
    let cx = x + 232;
    let cw = w.saturating_sub(232 + 28);
    let list_y = y + 74;
    let row_h = 26;
    let rows = h.saturating_sub(74 + 110) / row_h;
    let by = y + h - 56;
    Geom { x, y, w, h, cx, cw, list_y, row_h, rows, back: (cx, by, 110, 36), next: (x + w - 28 - 170, by, 170, 36) }
}

fn hit(r: (usize, usize, usize, usize), px: usize, py: usize) -> bool {
    px >= r.0 && px < r.0 + r.2 && py >= r.1 && py < r.1 + r.3
}

fn field_rect(g: &Geom, i: usize) -> (usize, usize, usize, usize) {
    (g.cx, g.list_y + 18 + i * 62, g.cw.min(380), 34)
}

fn toggle_rect(g: &Geom) -> (usize, usize, usize, usize) {
    (g.cx, g.list_y + 10, g.cw.min(420), 40)
}

// ── Rendering ──────────────────────────────────────────────────────────────

fn draw_button(fb: &FrameBuffer, r: (usize, usize, usize, usize), label: &str, primary: bool, enabled: bool) {
    let bg = if !enabled { Color::SURFACE_3 } else if primary { Color::ACCENT } else { Color::CARD };
    let fg = if !enabled { Color::TEXT_DIM } else if primary { Color::WHITE } else { Color::INK };
    fb.fill_rounded_rect(r.0, r.1, r.2, r.3, crate::eds::RADIUS_M, bg);
    if !primary {
        fb.draw_border(r.0, r.1, r.2, r.3, 1, Color::BORDER);
    }
    let tw = text::width_px(label, 14.0);
    text::draw_px(fb, r.0 + r.2.saturating_sub(tw) / 2, r.1 + 9, label, fg, 14.0);
}

fn draw_list(fb: &FrameBuffer, g: &Geom, items: &[(String, String, bool)], sel: usize) {
    // items: (label, right-hand note, enabled)
    let first = sel.saturating_sub(g.rows.saturating_sub(1));
    for (row, (label, note, enabled)) in items.iter().enumerate().skip(first).take(g.rows.max(1)) {
        let ry = g.list_y + (row - first) * g.row_h;
        if row == sel {
            fb.fill_rounded_rect(g.cx, ry, g.cw, g.row_h - 2, crate::eds::RADIUS_S, Color::SURFACE_3);
            fb.fill_rect(g.cx, ry + 4, 3, g.row_h - 10, Color::ACCENT);
        }
        let col = if *enabled { Color::INK } else { Color::TEXT_DIM };
        text::draw_px(fb, g.cx + 14, ry + 5, label, col, 13.5);
        if !note.is_empty() {
            let nw = text::width_px(note, 12.5);
            text::draw_px(fb, g.cx + g.cw - 14 - nw, ry + 6, note, Color::TEXT_SEC, 12.5);
        }
    }
}

fn draw_field(fb: &FrameBuffer, g: &Geom, i: usize, label: &str, value: &str, masked: bool, focused: bool) {
    let r = field_rect(g, i);
    text::draw_px(fb, r.0, r.1 - 18, label, Color::TEXT_DIM, 12.0);
    fb.fill_rounded_rect(r.0, r.1, r.2, r.3, crate::eds::RADIUS_S, if focused { Color::SURFACE_3 } else { Color::CARD });
    fb.draw_border(r.0, r.1, r.2, r.3, 1, Color::BORDER);
    if focused {
        fb.fill_rect(r.0, r.1 + r.3 - 2, r.2, 2, Color::ACCENT);
    }
    let mut shown = if masked { value.chars().map(|_| '\u{2022}').collect::<String>() } else { value.to_string() };
    if focused {
        shown.push('|');
    }
    text::draw_px(fb, r.0 + 10, r.1 + 9, &shown, Color::INK, 14.0);
}

fn draw_lines(fb: &FrameBuffer, x: usize, mut y: usize, lines: &[&str], col: Color, px: f32) -> usize {
    for l in lines {
        text::draw_px(fb, x, y, l, col, px);
        y += (px as usize) + 7;
    }
    y
}

/// Render the wizard into its window (called by the compositor).
pub fn render(fb: &FrameBuffer, win_x: usize, win_y: usize, win_w: usize, win_h: usize) {
    let g = geom(win_x, win_y, win_w, win_h);
    let wz = WIZARD.lock();
    fb.fill_rect(g.x, g.y, g.w, g.h, Color::SURFACE);

    // ── Sidebar: the steps ──
    fb.fill_rect(g.x, g.y, 210, g.h, Color::CARD);
    fb.fill_rect(g.x + 210, g.y, 1, g.h, Color::BORDER);
    text::draw_px(fb, g.x + 22, g.y + 18, "EuroInstall", Color::INK, 18.0);
    let cur = wz.page.index();
    for (i, name) in PAGE_NAMES.iter().enumerate() {
        let sy = g.y + 58 + i * 30;
        let (col, bg) = if i == cur {
            (Color::INK, Some(Color::SURFACE_3))
        } else if i < cur {
            (Color::SUCCESS, None)
        } else {
            (Color::TEXT_DIM, None)
        };
        if let Some(bg) = bg {
            fb.fill_rounded_rect(g.x + 12, sy - 4, 186, 26, crate::eds::RADIUS_S, bg);
        }
        text::draw_px(fb, g.x + 22, sy, &alloc::format!("{}  {name}", i + 1), col, 13.0);
        if i < cur {
            fb.fill_rounded_rect(g.x + 180, sy + 4, 8, 8, 2, Color::SUCCESS);
        }
    }
    let foot = if crate::instexec::media_available() { "media: own ESP (ok)" } else { "media: not found" };
    text::draw_px(fb, g.x + 22, g.y + g.h - 28, foot, Color::TEXT_DIM, 11.0);

    // ── Content ──
    let title_y = g.y + 18;
    let body_y = g.y + 48;
    let mut footer = true;
    let mut next_label = "Next";
    let mut back_enabled = true;
    match wz.page {
        Page::Welcome => {
            text::draw_px(fb, g.cx, title_y, "Install EuroOS on this computer", Color::INK, 22.0);
            draw_lines(
                fb,
                g.cx,
                body_y + 10,
                &[
                    "This wizard installs EuroOS on a disk of this computer.",
                    "The live system you are using now stays in memory.",
                    "Nothing is written to any disk until you confirm",
                    "on the summary page.",
                    "",
                    "You choose a language and keyboard, pick a blank disk,",
                    "create your account and decide on disk encryption.",
                    "",
                    "Enter = next, Esc = back, arrows = choose, Tab = field.",
                ],
                Color::TEXT_SEC,
                13.0,
            );
            back_enabled = false;
        }
        Page::Language => {
            text::draw_px(fb, g.cx, title_y, "Language", Color::INK, 22.0);
            text::draw_px(fb, g.cx, body_y, "Formats and system language (EuroLocale).", Color::TEXT_SEC, 13.0);
            let items: Vec<(String, String, bool)> = LANGS.iter().map(|(t, n)| (n.to_string(), t.to_string(), true)).collect();
            draw_list(fb, &g, &items, wz.lang);
        }
        Page::Keyboard => {
            text::draw_px(fb, g.cx, title_y, "Keyboard layout", Color::INK, 22.0);
            text::draw_px(fb, g.cx, body_y, "Applied on Next: the account page is typed in this layout.", Color::TEXT_SEC, 13.0);
            let items: Vec<(String, String, bool)> = KEYMAPS.iter().map(|(t, n)| (n.to_string(), t.to_string(), true)).collect();
            draw_list(fb, &g, &items, wz.keymap);
        }
        Page::Disk => {
            text::draw_px(fb, g.cx, title_y, "Target disk", Color::INK, 22.0);
            text::draw_px(fb, g.cx, body_y, "The chosen disk is erased. Disks in use cannot be chosen.", Color::TEXT_SEC, 13.0);
            if wz.targets.is_empty() {
                text::draw_px(fb, g.cx, g.list_y + 6, "No disks found.", Color::TEXT_DIM, 13.5);
            }
            let items: Vec<(String, String, bool)> = wz
                .targets
                .iter()
                .map(|t| {
                    let note = if !t.blank {
                        alloc::format!("{} · in use", size_label(t.mib))
                    } else if !t.selectable() {
                        alloc::format!("{} · too small", size_label(t.mib))
                    } else {
                        alloc::format!("{} · blank", size_label(t.mib))
                    };
                    (t.name.clone(), note, t.selectable())
                })
                .collect();
            draw_list(fb, &g, &items, wz.disk);
            let min = size_label(euroinstall::minimum_disk_bytes() / MIB);
            text::draw_px(fb, g.cx, g.y + g.h - 92, &alloc::format!("GPT: ESP + EuroOS-A + EuroOS-B + EuroVar. Minimum {min}."), Color::TEXT_DIM, 11.5);
        }
        Page::Account => {
            text::draw_px(fb, g.cx, title_y, "Your account", Color::INK, 22.0);
            text::draw_px(fb, g.cx, body_y, "The first account is the administrator (EuroID, Argon2id).", Color::TEXT_SEC, 13.0);
            draw_field(fb, &g, 0, "Computer name", &wz.hostname, false, wz.field == 0);
            draw_field(fb, &g, 1, "Username (lowercase)", &wz.username, false, wz.field == 1);
            draw_field(fb, &g, 2, "Password", &wz.password, true, wz.field == 2);
            draw_field(fb, &g, 3, "Repeat password", &wz.password2, true, wz.field == 3);
            text::draw_px(
                fb,
                g.cx,
                g.list_y + 18 + 4 * 62 - 14,
                "Policy: 12+ characters, upper + lower case, a digit and a symbol.",
                Color::TEXT_DIM,
                11.5,
            );
        }
        Page::Encryption => {
            text::draw_px(fb, g.cx, title_y, "Full-disk encryption", Color::INK, 22.0);
            let r = toggle_rect(&g);
            if crate::tpm::present() {
                text::draw_px(fb, g.cx, body_y, "The disk key is sealed to this computer's TPM and measured boot.", Color::TEXT_SEC, 13.0);
                fb.fill_rounded_rect(r.0, r.1, r.2, r.3, crate::eds::RADIUS_S, if wz.fde { Color::SUCCESS_SOFT } else { Color::CARD });
                fb.draw_border(r.0, r.1, r.2, r.3, 1, Color::BORDER);
                let box_mark = if wz.fde { "[x]" } else { "[ ]" };
                text::draw_px(fb, r.0 + 12, r.1 + 11, &alloc::format!("{box_mark}  Encrypt the disk (EuroFDE, TPM-sealed key)"), Color::INK, 14.0);
                text::draw_px(fb, g.cx, r.1 + r.3 + 14, "Space or a click toggles. A tampered boot cannot unseal the key.", Color::TEXT_DIM, 11.5);
            } else {
                text::draw_px(fb, g.cx, body_y, "No TPM was found on this computer.", Color::TEXT_SEC, 13.0);
                draw_lines(
                    fb,
                    g.cx,
                    r.1 + 6,
                    &[
                        "EuroOS seals the disk key to a TPM and refuses a plaintext",
                        "fallback, so encryption is not available on this machine.",
                        "The installation continues without encryption.",
                        "A recovery-passphrase mode is planned.",
                    ],
                    Color::TEXT_SEC,
                    13.0,
                );
            }
        }
        Page::Summary => {
            text::draw_px(fb, g.cx, title_y, "Ready to install", Color::INK, 22.0);
            text::draw_px(fb, g.cx, body_y, "Check the choices. Install erases the disk and writes EuroOS.", Color::TEXT_SEC, 13.0);
            let cfg = wz.config();
            let target = wz.targets.get(wz.disk).map(|t| alloc::format!("{} ({})", t.name, size_label(t.mib))).unwrap_or_default();
            let rows = [
                ("Target disk", target),
                ("Language", cfg.locale.clone()),
                ("Keyboard", cfg.keymap.clone()),
                ("Computer name", cfg.hostname.clone()),
                ("Administrator", cfg.username.clone()),
                ("Encryption", String::from(if cfg.fde { "on (TPM-sealed)" } else { "off" })),
            ];
            let mut ry = g.list_y;
            for (k, v) in rows.iter() {
                text::draw_px(fb, g.cx, ry, k, Color::TEXT_DIM, 12.5);
                text::draw_px(fb, g.cx + 150, ry, v, Color::INK, 12.5);
                ry += 24;
            }
            ry += 6;
            match plan(&cfg) {
                Ok(steps) => {
                    text::draw_px(fb, g.cx, ry, &alloc::format!("Plan: {} steps", steps.len()), Color::INK, 12.5);
                    ry += 20;
                    let shown: Vec<String> = steps.iter().take(6).map(euroinstall::describe).collect();
                    for s in shown {
                        text::draw_px(fb, g.cx + 12, ry, &alloc::format!("\u{2022} {s}"), Color::TEXT_SEC, 11.5);
                        ry += 17;
                    }
                    if steps.len() > 6 {
                        text::draw_px(fb, g.cx + 12, ry, &alloc::format!("\u{2022} and {} more", steps.len() - 6), Color::TEXT_DIM, 11.5);
                    }
                }
                Err(e) => text::draw_px(fb, g.cx, ry, &alloc::format!("plan error: {e:?}"), Color::RED, 12.5),
            }
            next_label = "Install";
        }
        Page::Installing => {
            text::draw_px(fb, g.cx, title_y, "Installing EuroOS", Color::INK, 22.0);
            text::draw_px(fb, g.cx, body_y, "Writing the disk. This takes a few minutes; do not power off.", Color::TEXT_SEC, 13.0);
            let mut ly = g.list_y;
            let start = wz.log.len().saturating_sub(10);
            for (i, l) in wz.log.iter().enumerate().skip(start) {
                let last = i + 1 == wz.log.len();
                fb.fill_rounded_rect(g.cx, ly + 5, 9, 9, 2, if last { Color::ACCENT } else { Color::SUCCESS });
                text::draw_px(fb, g.cx + 22, ly, l, Color::INK, 13.0);
                ly += 24;
            }
            footer = false;
        }
        Page::Done => {
            text::draw_px(fb, g.cx, title_y, "Installation complete", Color::INK, 22.0);
            draw_lines(
                fb,
                g.cx,
                body_y + 10,
                &[
                    &alloc::format!("EuroOS is installed on {}.", wz.installed_to),
                    "",
                    "Remove the installation medium, then restart.",
                    "The computer boots into the new installation and",
                    "asks for the password you chose.",
                ],
                Color::TEXT_SEC,
                13.0,
            );
            fb.fill_rounded_rect(g.cx, g.list_y + 120, g.cw.min(380), 32, crate::eds::RADIUS_S, Color::SUCCESS_SOFT);
            text::draw_px(fb, g.cx + 12, g.list_y + 128, "Verified after a remount: GPT, ESP, root, account", Color::SUCCESS, 12.5);
            next_label = "Restart now";
            back_enabled = false;
        }
        Page::Failed => {
            text::draw_px(fb, g.cx, title_y, "Installation failed", Color::RED, 22.0);
            text::draw_px(fb, g.cx, body_y, "Nothing more was written. Details: serial log.", Color::TEXT_SEC, 13.0);
            let mut ly = g.list_y;
            for l in wz.log.iter() {
                text::draw_px(fb, g.cx, ly, l, Color::TEXT_SEC, 12.5);
                ly += 20;
            }
            next_label = "Back to summary";
            back_enabled = false;
        }
    }

    if !wz.error.is_empty() {
        text::draw_px(fb, g.cx, g.y + g.h - 76, &wz.error, Color::RED, 12.5);
    }
    if footer {
        draw_button(fb, g.back, "Back", false, back_enabled);
        draw_button(fb, g.next, next_label, true, true);
    }
}

/// Repaint the wizard content straight to the screen (used while an install
/// blocks the desktop loop, so progress stays visible).
fn paint_now(fb: &FrameBuffer, win: (usize, usize, usize, usize)) {
    render(fb, win.0, win.1, win.2, win.3);
    fb.present();
    if let Some((bb, bw, bh, bs)) = fb.backbuffer() {
        crate::virtio_gpu::present_frame(bb, bw, bh, bs);
    }
}

fn run_install(fb: &FrameBuffer, win: (usize, usize, usize, usize)) {
    let (cfg, bus, name) = {
        let wz = WIZARD.lock();
        let t = wz.targets.get(wz.disk).cloned();
        (wz.config(), t.as_ref().map(|t| t.bus), t.map(|t| t.name).unwrap_or_default())
    };
    let progress = |msg: &str| {
        crate::serial_println!("[wizard] {msg}");
        WIZARD.lock().log.push(msg.to_string());
        paint_now(fb, win);
    };
    progress(&alloc::format!("Target: {name}"));
    let ok = match bus {
        Some(Bus::Virtio(d)) => crate::instexec::install_to_disk(d, &cfg, &progress),
        Some(Bus::Nvme) => crate::instexec::install_to_nvme(&cfg, &progress),
        Some(Bus::Ahci(i)) => crate::instexec::install_to_ahci(i, &cfg, &progress),
        None => false,
    };
    let mut wz = WIZARD.lock();
    if ok {
        wz.installed_to = name;
        wz.page = Page::Done;
    } else {
        wz.page = Page::Failed;
    }
    // The typed secret is not kept around after the install.
    wz.password.clear();
    wz.password2.clear();
}

fn act(fb: &FrameBuffer, win: (usize, usize, usize, usize), action: Option<Action>) {
    match action {
        Some(Action::Install) => run_install(fb, win),
        Some(Action::Reboot) => crate::power::reboot(),
        None => {}
    }
}

/// A key for the focused wizard window.
pub fn key(fb: &FrameBuffer, win_x: usize, win_y: usize, win_w: usize, win_h: usize, k: Key) {
    let action = WIZARD.lock().handle_key(k);
    act(fb, (win_x, win_y, win_w, win_h), action);
}

/// A left click at (px, py) in the wizard window. Returns true if it hit a control.
pub fn click(fb: &FrameBuffer, win_x: usize, win_y: usize, win_w: usize, win_h: usize, px: usize, py: usize) -> bool {
    let g = geom(win_x, win_y, win_w, win_h);
    let win = (win_x, win_y, win_w, win_h);
    let mut action = None;
    let handled = {
        let mut wz = WIZARD.lock();
        if wz.page == Page::Installing {
            return false;
        }
        if hit(g.next, px, py) {
            action = wz.next();
            true
        } else if hit(g.back, px, py) && !matches!(wz.page, Page::Welcome | Page::Done | Page::Failed) {
            wz.back();
            true
        } else {
            match wz.page {
                Page::Language | Page::Keyboard | Page::Disk => {
                    let n = wz.list_len();
                    if n > 0 && px >= g.cx && px < g.cx + g.cw && py >= g.list_y && py < g.list_y + g.row_h * n.min(g.rows.max(1)) {
                        let sel = match wz.page {
                            Page::Language => wz.lang,
                            Page::Keyboard => wz.keymap,
                            _ => wz.disk,
                        };
                        let first = sel.saturating_sub(g.rows.saturating_sub(1));
                        let row = first + (py - g.list_y) / g.row_h;
                        if row < n {
                            if let Some(s) = wz.sel_mut() {
                                *s = row;
                            }
                        }
                        true
                    } else {
                        false
                    }
                }
                Page::Account => {
                    let mut hitf = false;
                    for i in 0..4 {
                        if hit(field_rect(&g, i), px, py) {
                            wz.field = i;
                            hitf = true;
                        }
                    }
                    hitf
                }
                Page::Encryption => {
                    if crate::tpm::present() && hit(toggle_rect(&g), px, py) {
                        wz.fde = !wz.fde;
                        true
                    } else {
                        false
                    }
                }
                _ => false,
            }
        }
    };
    act(fb, win, action);
    handled
}

// ── Shell path + planner self-test (unchanged behaviour) ───────────────────

fn sample_config(live: bool) -> Config {
    let mut c = crate::instexec::default_config();
    c.live = live;
    c
}

pub fn selftest() {
    let install = plan(&sample_config(false));
    let live = plan(&sample_config(true));

    let (steps_ok, parts_ok) = match &install {
        Ok(steps) => {
            let fde = steps.iter().position(|s| *s == Step::EnrollFde);
            let fmt = steps.iter().position(|s| *s == Step::FormatSystem);
            let order_ok = matches!((fde, fmt), (Some(f), Some(m)) if f < m)
                && matches!(steps.first(), Some(Step::Partition(_)))
                && steps.last() == Some(&Step::FinalizeBoot);
            let parts_ok = if let Some(Step::Partition(p)) = steps.first() {
                p.len() == 4 && p.windows(2).all(|w| w[0].start_lba + w[0].sectors <= w[1].start_lba)
            } else {
                false
            };
            (order_ok, parts_ok)
        }
        Err(_) => (false, false),
    };

    let live_ok = live
        .as_ref()
        .map(|s| !s.iter().any(|st| matches!(st, Step::Partition(_) | Step::FormatSystem | Step::WriteKernelSlots)))
        .unwrap_or(false);

    // The wizard's language list must be accepted by the planner.
    let langs_ok = LANGS.iter().all(|(t, _)| eurolocale::Lang::parse(t).is_some());
    let keymaps_ok = KEYMAPS.iter().all(|(t, _)| eurokeymap::Layout::parse(t).is_some());

    let ok = steps_ok && parts_ok && live_ok && langs_ok && keymaps_ok;
    let nsteps = install.as_ref().map(|s| s.len()).unwrap_or(0);
    crate::serial_println!(
        "[install] EuroInstall planner: {nsteps}-step A/B install (FDE before format={steps_ok}, 4 non-overlapping partitions={parts_ok}), live mode leaves disk alone={live_ok}, wizard lists valid (langs={langs_ok}, keymaps={keymaps_ok}) → {}",
        if ok { "OK (guided install + live image, host-tested planner) ✓" } else { "FAILED" }
    );
}

pub fn shell(line: &str) -> Vec<String> {
    let toks: Vec<&str> = line.split_whitespace().collect();
    let args: Vec<&str> = if toks.first() == Some(&"euroinstall") { toks[1..].to_vec() } else { toks };

    let (mut target, mut hostname, mut user, mut live) = (None::<usize>, None::<String>, None::<String>, false);
    let mut i = 0;
    while i < args.len() {
        match args[i] {
            "--to" => { target = args.get(i + 1).and_then(|v| v.parse().ok()); i += 2; continue; }
            "--hostname" => { hostname = args.get(i + 1).map(|s| String::from(*s)); i += 2; continue; }
            "--user" => { user = args.get(i + 1).map(|s| String::from(*s)); i += 2; continue; }
            "live" => live = true,
            _ => {}
        }
        i += 1;
    }

    if let Some(dev) = target {
        return do_install(dev, hostname, user);
    }

    let cfg = sample_config(live);
    let mut out = alloc::vec![alloc::format!(
        "EuroInstall \u{2014} {} (dry-run, nothing is written):",
        if live { "live image (RAM-only)" } else { "install to disk (A/B)" }
    )];
    match plan(&cfg) {
        Ok(steps) => {
            for (i, s) in steps.iter().enumerate() {
                out.push(alloc::format!("  {:>2}. {}", i + 1, euroinstall::describe(s)));
            }
        }
        Err(e) => out.push(alloc::format!("  config invalid: {e:?}")),
    }
    let n = crate::virtio_blk::device_count();
    if n == 0 {
        out.push(String::from("  target disks: (no virtio-blk disk attached)"));
    } else {
        out.push(String::from("  target disks:"));
        for d in 0..n {
            let mib = crate::virtio_blk::capacity_sectors_dev(d) * 512 / 1024 / 1024;
            let blank = if crate::instexec::disk_is_blank(d) { "blank" } else { "in use" };
            out.push(alloc::format!("    virtio-blk {d}  \u{00b7}  {mib} MiB  \u{00b7}  {blank}"));
        }
    }
    let media = if crate::instexec::media_available() { "yes" } else { "no (only after a UEFI boot)" };
    out.push(alloc::format!("  install-media: {media}"));
    out.push(String::from("  REAL install:  euroinstall --to <N> [--hostname H] [--user U]   \u{26A0} ERASES disk N (dev password; use the desktop wizard for a real account)"));
    out
}

fn do_install(dev: usize, hostname: Option<String>, user: Option<String>) -> Vec<String> {
    if !crate::instexec::media_available() {
        return alloc::vec![String::from("euroinstall: no install media \u{2014} only possible from a real UEFI boot.")];
    }
    if !crate::virtio_blk::present_dev(dev) {
        return alloc::vec![alloc::format!("euroinstall: no virtio-blk disk {dev} (see `euroinstall` for the list).")];
    }
    let mut cfg = crate::instexec::default_config();
    if let Some(h) = hostname { cfg.hostname = h; }
    if let Some(u) = user { cfg.username = u; }
    let mib = crate::virtio_blk::capacity_sectors_dev(dev) * 512 / 1024 / 1024;
    let ok = crate::instexec::install_to_disk(dev, &cfg, &|_| {});
    alloc::vec![
        alloc::format!("EuroInstall \u{2192} virtio-blk {dev} ({mib} MiB) \u{00b7} hostname={} \u{00b7} user={}", cfg.hostname, cfg.username),
        if ok {
            String::from("  \u{2713} bootable + provisioned EuroOS written \u{2014} the disk now boots standalone (see [q1x3]).")
        } else {
            String::from("  \u{2717} install failed (disk too small, no media, or write error).")
        },
    ]
}
