//! Interop: eurodocio opens a REAL `.docx` (produced by python's zipfile with
//! real DEFLATE) and its `save()` output is read back by real tools (python).

use std::process::Command;

use eurodocio::docx;
use eurodocio::zip;

const REAL_DOCX: &[u8] = include_bytes!("real.docx");

#[test]
fn opens_a_real_docx_from_a_real_tool() {
    // The ZIP layer sees the same parts a real reader does.
    let names: Vec<String> = zip::read(REAL_DOCX).unwrap().into_iter().map(|e| e.name).collect();
    assert!(names.iter().any(|n| n == "word/document.xml"));
    assert!(names.iter().any(|n| n == "[Content_Types].xml"));

    // And the OOXML body decodes to the expected text (DEFLATE-inflated).
    let blocks = docx::open(REAL_DOCX).expect("open real .docx");
    let text = docx::plain_text(&blocks);
    assert!(text.contains("EuroOS reads real Office files."), "got: {text:?}");
    assert!(text.contains("Bold heading paragraph"), "got: {text:?}");
}

#[test]
fn our_saved_docx_is_read_by_real_tools() {
    let blocks = vec![
        eurodoc::model::Block::Paragraph(eurodoc::model::Paragraph::text("Written by EuroSuite.")),
        eurodoc::model::Block::Paragraph(eurodoc::model::Paragraph::text("Second line here.")),
    ];
    let out = docx::save(&blocks);

    if Command::new("python3").arg("--version").output().is_err() {
        eprintln!("python3 not available — skipping real-tool read-back");
        return;
    }
    // Write to a temp file and have python's zipfile open it + read document.xml.
    let dir = std::env::temp_dir();
    let path = dir.join("eurosuite_out.docx");
    std::fs::write(&path, &out).unwrap();
    let script = format!(
        "import zipfile,sys\n\
         z=zipfile.ZipFile(r'{}')\n\
         assert 'word/document.xml' in z.namelist(), z.namelist()\n\
         x=z.read('word/document.xml').decode()\n\
         assert 'Written by EuroSuite.' in x and 'Second line here.' in x, x\n\
         print('OK')",
        path.display()
    );
    let output = Command::new("python3").arg("-c").arg(&script).output().unwrap();
    assert!(
        output.status.success(),
        "real zipfile could not read our .docx: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "OK");
}
