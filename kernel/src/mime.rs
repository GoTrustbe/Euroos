//! Kernel side of **EuroMime** (3F-5): the desktop's MIME + default-app layer.
//! It answers "what is this file, and which app opens it" for the file manager
//! and the `open` command. Detection (magic bytes + extension) and the
//! association table are host-tested in [`euromime`]; here we hold the live
//! [`Registry`], run `[3f5]`, and resolve real files on the live FS.

use alloc::string::String;
use alloc::vec::Vec;

use eurofs::FileSystem;
use euromime::Registry;
use spin::Mutex;

static REGISTRY: Mutex<Option<Registry>> = Mutex::new(None);

fn with_registry<R>(f: impl FnOnce(&mut Registry) -> R) -> R {
    let mut g = REGISTRY.lock();
    let r = g.get_or_insert_with(Registry::with_defaults);
    f(r)
}

/// The MIME type + default app for a real file on `fs` (reads a prefix for the
/// magic sniff). Returns `(mime, Some(app)|None)`.
pub fn resolve(fs: &mut dyn FileSystem, path: &str) -> (String, Option<String>) {
    let data = fs.read_file(path).unwrap_or_default();
    let head = &data[..data.len().min(64)];
    let mime = euromime::detect_refined(path, head);
    let app = with_registry(|r| r.default_app(&mime).map(String::from));
    (mime, app)
}

/// Set the default app for a MIME type (settings / `open --set`).
pub fn set_default(mime: &str, app: &str) {
    with_registry(|r| r.set_default(mime, app));
}

/// `[3f5]` boot self-test — write real files to the live FS, then prove magic
/// beats extension, a real `.docx` resolves to Writer, an image to the viewer, a
/// text file to the editor, and a user override changes the default.
pub fn selftest(fs: &mut dyn FileSystem) {
    let _ = fs.create_dir("/mimetest");
    // A PNG mislabelled .txt → magic wins.
    let png = [0x89u8, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 0];
    let _ = fs.write_file("/mimetest/photo.txt", &png);
    // A real .docx (ZIP magic) → refined to the Office type → Writer.
    let docx = eurodocio::docx::save(&alloc::vec![eurodoc::model::Block::Paragraph(
        eurodoc::model::Paragraph::text("hi")
    )]);
    let _ = fs.write_file("/mimetest/report.docx", &docx);
    let _ = fs.write_file("/mimetest/notes.md", b"# EuroOS");

    let (png_mime, png_app) = resolve(fs, "/mimetest/photo.txt");
    let magic_wins = png_mime == "image/png" && png_app.as_deref() == Some("euroshot");
    let (docx_mime, docx_app) = resolve(fs, "/mimetest/report.docx");
    let docx_ok = docx_mime.contains("wordprocessingml") && docx_app.as_deref() == Some("eurowriter");
    let (_txt_mime, txt_app) = resolve(fs, "/mimetest/notes.md");
    let txt_ok = txt_app.as_deref() == Some("eurotext");

    // User override: make text open in Writer instead.
    set_default("text/plain", "eurowriter");
    let (_m, overridden) = resolve(fs, "/mimetest/notes.md");
    let override_ok = overridden.as_deref() == Some("eurowriter");
    set_default("text/plain", "eurotext"); // restore

    // Cleanup.
    let _ = fs.remove_file("/mimetest/photo.txt");
    let _ = fs.remove_file("/mimetest/report.docx");
    let _ = fs.remove_file("/mimetest/notes.md");

    let ok = magic_wins && docx_ok && txt_ok && override_ok;
    crate::serial_println!(
        "[3f5] MIME + default-app (euromime): magic-beats-extension(png-as-.txt→euroshot)={magic_wins}, real-.docx→eurowriter={docx_ok}, text→eurotext={txt_ok}, user-override={override_ok} → {}",
        if ok { "OK (open-with routing on the live FS) ✓" } else { "FAILED ✗" }
    );
}

/// `open <path>` shell command: detect the type of a real file and show which
/// app opens it (the file-manager double-click, in the shell).
/// `open --set <mime> <app>` changes a default association.
pub fn shell(fs: &mut dyn FileSystem, arg1: &str, arg2: &str) -> Vec<String> {
    if arg1 == "--set" {
        let mut it = arg2.split_whitespace();
        match (it.next(), it.next()) {
            (Some(mime), Some(app)) => {
                set_default(mime, app);
                alloc::vec![alloc::format!("default for {mime} → {app}")]
            }
            _ => alloc::vec![String::from("usage: open --set <mime> <app>")],
        }
    } else if arg1.is_empty() {
        alloc::vec![String::from("usage: open <path>   |   open --set <mime> <app>")]
    } else if !fs.exists(arg1) {
        alloc::vec![alloc::format!("open: '{arg1}' not found")]
    } else {
        let (mime, app) = resolve(fs, arg1);
        match app {
            Some(a) => alloc::vec![
                alloc::format!("{arg1}: {mime}"),
                alloc::format!("  opens with: {a}"),
            ],
            None => alloc::vec![
                alloc::format!("{arg1}: {mime}"),
                String::from("  no default app (open --set <mime> <app>)"),
            ],
        }
    }
}
