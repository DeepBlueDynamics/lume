#![cfg(feature = "pdf")]
use std::{io::Write, path::PathBuf, process::Command};
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("documents-{}-{}",std::process::id(),lume::uuid_v4()));
        std::fs::create_dir_all(&root).unwrap();
        Self(root)
    }
}
impl Drop for Fixture { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }
fn pdf(path: &std::path::Path, encrypted: bool) {
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Count 2 /Kids [4 0 R 6 0 R] >>".into(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 3 0 R >> >> /Contents 5 0 R >>".into(),
        { let text = "BT /F1 12 Tf 72 720 Td (Bilge pump inspection procedure.) Tj ET";
          format!("<< /Length {} >>\nstream\n{text}\nendstream",text.len()) },
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 7 0 R >>".into(),
        "<< /Length 0 >>\nstream\n\nendstream".into(),
        "<< /Filter /Standard /V 2 /R 3 /Length 128 /O <00000000> /U <00000000> /P -4 >>".into(),
    ];
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (i, object) in objects.iter().enumerate() {
        offsets.push(bytes.len());
        write!(bytes,"{} 0 obj\n{object}\nendobj\n",i+1).unwrap();
    }
    let xref = bytes.len();
    write!(bytes,"xref\n0 9\n0000000000 65535 f \n").unwrap();
    for offset in offsets { writeln!(bytes,"{offset:010} 00000 n ").unwrap(); }
    let encryption = if encrypted { "/Encrypt 8 0 R /ID [<00000000> <00000000>]" } else { "" };
    write!(bytes,"trailer\n<< /Size 9 /Root 1 0 R {encryption} >>\nstartxref\n{xref}\n%%EOF\n").unwrap();
    std::fs::write(path,bytes).unwrap();
}
fn epub(path: &std::path::Path) { epub_with_encryption(path,None); }
fn epub_with_encryption(path: &std::path::Path, encrypted: Option<&str>) {
    let file = std::fs::File::create(path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    for (name,text) in [
        ("META-INF/container.xml",r#"<container><rootfiles><rootfile full-path="OEBPS/book.opf"/></rootfiles></container>"#),
        ("OEBPS/book.opf",r#"<package><manifest><item id="a" href="a.xhtml" media-type="application/xhtml+xml"/><item id="b" href="b.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="b"/><itemref idref="a"/></spine></package>"#),
        ("OEBPS/a.xhtml","<html><head><title>Chapter A</title></head><body><h1>A</h1><p>Anchor maintenance.</p><script>danger()</script></body></html>"),
        ("OEBPS/b.xhtml","<html><head><title>Chapter B</title></head><body><h1>B</h1><p>Bilge pump maintenance.</p></body></html>"),
    ] {
        zip.start_file(name,zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)).unwrap();
        zip.write_all(text.as_bytes()).unwrap();
    }
    if let Some(uri) = encrypted {
        zip.start_file("META-INF/encryption.xml", zip::write::SimpleFileOptions::default()).unwrap();
        write!(zip, "<encryption><EncryptedData><CipherData><CipherReference URI=\"{uri}\"/></CipherData></EncryptedData></encryption>").unwrap();
    }
    zip.finish().unwrap();
}
#[test]
fn generated_pdf_retains_pages_and_skips_empty_page() {
    let fixture = Fixture::new();
    let path = fixture.0.join("manual.pdf");
    pdf(&path,false);
    let result = lume::document_extract::worker(&path).unwrap();
    assert_eq!(result.sections.len(),1);
    assert_eq!(result.sections[0].title,"Page 1");
    assert_eq!(result.sections[0].line_number,1);
    assert_eq!(result.sections[0].filename.as_deref(),path.to_str());
    assert!(result.sections[0].body.contains("Bilge pump"));
    assert_eq!(result.skipped_units,1);
}
#[test]
fn epub_spine_order_and_existing_html_cleaner() {
    let fixture = Fixture::new();
    let path = fixture.0.join("book.epub");
    epub(&path);
    let result = lume::document_extract::worker(&path).unwrap();
    assert_eq!(result.sections.len(),2);
    assert_eq!(result.sections[0].title,"Chapter B");
    assert_eq!(result.sections[1].title,"Chapter A");
    assert!(!result.sections[1].body.contains("danger()"));
    assert!(!result.sections[0].body.contains("<p>"));
}
#[test]
fn epub_skips_encrypted_chapters_but_not_font_obfuscation() {
    let fixture = Fixture::new();
    let path = fixture.0.join("encrypted.epub");
    epub_with_encryption(&path,Some("OEBPS/b.xhtml"));
    let report = lume::document_extract::worker(&path).unwrap();
    assert_eq!(report.skipped_units,1);
    assert_eq!(report.sections.len(),1);
    assert_eq!(report.sections[0].title,"Chapter A");
    epub_with_encryption(&path,Some("OEBPS/font.ttf"));
    assert_eq!(lume::document_extract::worker(&path).unwrap().sections.len(),2);
}
#[test]
fn index_falls_back_without_uv_and_continues_after_bad_documents() {
    let fixture = Fixture::new();
    let docs = fixture.0.join("docs");
    std::fs::create_dir_all(&docs).unwrap();
    pdf(&docs.join("manual.pdf"),false);
    pdf(&docs.join("encrypted.pdf"),true);
    epub(&docs.join("book.epub"));
    std::fs::write(docs.join("broken.pdf"),"not a PDF").unwrap();
    std::fs::write(docs.join("ordinary.md"),"## Safety\n\nLife jacket checks. Inspect straps and buckles, confirm inflation cartridges are fitted correctly and explain emergency use to everyone aboard before departure.\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_lume"))
        .arg("index").arg(&docs).arg("--db").arg(fixture.0.join("index"))
        .env("PATH",&fixture.0).output().unwrap();
    assert!(output.status.success(),"{}",String::from_utf8_lossy(&output.stderr));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("skipped"),"{stderr}");
    let index = lume::LoadedIndex::open(&fixture.0.join("index")).unwrap();
    assert!(index.bm25.sections.iter().any(|s| s.title=="Page 1" && s.body.contains("Bilge pump")));
    assert!(index.bm25.sections.iter().any(|s| s.title=="Chapter B"));
    assert!(index.bm25.sections.iter().any(|s| s.body.contains("Life jacket")));
}
#[cfg(unix)]
#[test]
fn available_uv_extractor_remains_preferred() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new();
    let docs = fixture.0.join("docs");
    std::fs::create_dir_all(&docs).unwrap();
    pdf(&docs.join("manual.pdf"),false);
    let uv = fixture.0.join("uv");
    std::fs::write(&uv, "#!/bin/sh\nprintf '%s' '{\"success\":true,\"total_pages\":2,\"pages\":[{\"page_number\":2,\"text\":\"Preferred UV bilge pump text.\"}]}'\n").unwrap();
    std::fs::set_permissions(&uv,std::fs::Permissions::from_mode(0o700)).unwrap();
    let script = fixture.0.join("extract.py");
    std::fs::write(&script,"# mock extractor").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_lume"))
        .arg("index").arg(&docs).arg("--db").arg(fixture.0.join("index"))
        .env("PATH",&fixture.0).env("LUME_EXTRACTOR_PATH",&script).output().unwrap();
    assert!(output.status.success(),"{}",String::from_utf8_lossy(&output.stderr));
    let index = lume::LoadedIndex::open(&fixture.0.join("index")).unwrap();
    assert_eq!(index.bm25.sections.len(),1);
    assert_eq!(index.bm25.sections[0].title,"Page 2");
    assert!(index.bm25.sections[0].body.contains("Preferred UV"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("skipped 1"));
}
#[test]
fn oversized_page_is_counted_and_skipped() {
    let fixture = Fixture::new();
    let path = fixture.0.join("page-limit.pdf");
    pdf(&path, false);
    let mut document = lopdf::Document::load(&path).unwrap();
    document.objects.insert((5,0), lopdf::Object::Stream(lopdf::Stream::new(
        lopdf::Dictionary::new(), vec![b' '; lume::document_extract::UNIT_LIMIT+1]
    )));
    document.save(&path).unwrap();
    let result = lume::document_extract::worker(&path).unwrap();
    assert!(result.sections.is_empty());
    assert_eq!(result.skipped_units,2);
}
#[test]
fn oversized_input_is_rejected_before_loading() {
    let fixture = Fixture::new();
    let path = fixture.0.join("large.pdf");
    std::fs::File::create(&path).unwrap().set_len(lume::document_extract::INPUT_LIMIT+1).unwrap();
    assert!(lume::document_extract::extract(&path,None).unwrap_err().contains("input"));
}
