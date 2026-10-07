use std::{io::Write, path::Path, time::Instant};
fn fixture(path: &Path) {
    let mut objects = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        String::new(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into(),
    ];
    let mut kids = Vec::new();
    for number in 1..=900 {
        let page = objects.len() + 1;
        kids.push(format!("{page} 0 R"));
        objects.push(format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 3 0 R >> >> /Contents {} 0 R >>", page+1));
        let body = format!("BT /F1 12 Tf 72 720 Td (Page {number}: bilge pump maintenance, navigation and safety.) Tj ET");
        objects.push(format!("<< /Length {} >>\nstream\n{body}\nendstream",body.len()));
    }
    objects[1] = format!("<< /Type /Pages /Count 900 /Kids [{}] >>",kids.join(" "));
    let mut pdf = b"%PDF-1.4\n".to_vec();
    let mut offsets = vec![0];
    for (i, object) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        write!(pdf,"{} 0 obj\n{}\nendobj\n",i+1,object).unwrap();
    }
    let xref = pdf.len();
    write!(pdf,"xref\n0 {}\n0000000000 65535 f \n",objects.len()+1).unwrap();
    for offset in offsets.iter().skip(1) { writeln!(pdf,"{offset:010} 00000 n ").unwrap(); }
    write!(pdf,"trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",objects.len()+1).unwrap();
    std::fs::write(path,pdf).unwrap();
}
fn normalized_hash(mut hash: u64, text: &str) -> u64 {
    for byte in text.split_whitespace().collect::<Vec<_>>().join(" ").bytes().chain([0]) {
        hash = (hash ^ u64::from(byte)).wrapping_mul(1099511628211);
    }
    hash
}
fn main() {
    let arg = std::env::args().nth(1).expect("PDF path");
    let path = Path::new(&arg);
    if !path.exists() { fixture(path); }
    let mut times = Vec::new();
    let mut chars = 0;
    let mut pages = 0;
    let mut checksum = 0u64;
    for _ in 0..6 {
        let start = Instant::now();
        #[cfg(feature="direct")]
        {
            let doc = lopdf::Document::load_with_options(path,lopdf::LoadOptions::with_max_decompressed_size(8 << 20)).unwrap();
            pages = doc.get_pages().len();
            chars = 0;
            checksum = 14695981039346656037;
            for page in doc.get_pages().keys() {
                let text = doc.extract_text_with_limit(&[*page],8 << 20).unwrap();
                chars += text.len();
                checksum = normalized_hash(checksum, &text);
            }
        }
        #[cfg(feature="extract")]
        {
            let output = pdf_extract::extract_text_by_pages(path).unwrap();
            pages = output.len();
            chars = output.iter().map(String::len).sum::<usize>();
            checksum = output.iter().fold(14695981039346656037, |hash,text| normalized_hash(hash,text));
        }
        times.push(start.elapsed().as_secs_f64()*1000.0);
    }
    let cold = times.remove(0);
    times.sort_by(f64::total_cmp);
    let bytes = std::fs::metadata(std::env::current_exe().unwrap()).unwrap().len();
    println!("{{\"pages\":{pages},\"characters\":{chars},\"normalized_checksum\":\"{checksum:016x}\",\"cold_ms\":{cold:.3},\"warm_p50_ms\":{:.3},\"warm_max_ms\":{:.3},\"release_binary_bytes\":{bytes}}}",times[2],times[4]);
}
