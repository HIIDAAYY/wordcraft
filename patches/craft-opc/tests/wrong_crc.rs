//! A part whose CRC-32 is wrong in both zip headers (as WPS Office writes its pictures) is read
//! as Word reads it, within the size limits.

use std::io::{Cursor, Write};

use craft_opc::{Limits, Package};

fn zip_with_wrong_crc(method: zip::CompressionMethod, content: &[u8]) -> Vec<u8> {
    let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default().compression_method(method);
    w.start_file("word/media/image1.png", opts).unwrap();
    w.write_all(content).unwrap();
    let mut out = w.finish().unwrap().into_inner();
    let crc = zip::ZipArchive::new(Cursor::new(&out)).unwrap().by_index(0).unwrap().crc32();
    let needle = crc.to_le_bytes();
    let mut patched = 0;
    let mut at = 0;
    while let Some(pos) = out[at..].windows(4).position(|w| w == needle) {
        let p = at + pos;
        out[p] ^= 0xFF;
        patched += 1;
        at = p + 4;
    }
    assert_eq!(patched, 2, "the CRC sits in the local header and the central directory");
    out
}

#[test]
fn parts_with_a_wrong_crc_are_read_as_word_does() {
    let content: Vec<u8> = (0..20_000u32).map(|n| (n % 251) as u8).collect();
    for method in [zip::CompressionMethod::Stored, zip::CompressionMethod::Deflated] {
        let bytes = zip_with_wrong_crc(method, &content);
        let pkg = Package::open(&bytes, &Limits::new(100, 1 << 20, 1 << 22)).unwrap();
        assert_eq!(pkg.get("word/media/image1.png"), Some(content.as_slice()), "{method:?}");
        assert!(pkg.skipped().is_empty());
    }
}
