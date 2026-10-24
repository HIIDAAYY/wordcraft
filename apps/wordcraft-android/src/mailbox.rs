//! Files the Java side of the app hands to Rust.
//!
//! The Android pickers, `ACTION_VIEW` ("Open with WordCraft") and the save picker all answer on
//! Java's UI thread, long after the Rust call that started them returned. Rather than calling back
//! into Rust across threads, the activity drops small files into the *inbox* folder, each written
//! under a `.tmp` name and renamed when complete, and the app drains them at the start of a frame.
//! Names sort in the order they were written.
//!
//! | file | content |
//! |---|---|
//! | `<seq>.open` | a picked or shared file: its display name, a newline, its bytes |
//! | `<seq>.done` | a picker closed (whether or not it delivered files) |
//! | `<seq>.save` | a save finished: the ticket, a newline, `ok`, `cancel` or `fail`, and for a failure a newline and the reason |
//! | `<seq>.text` | text the on-screen keyboard typed that has no key code (accented letters, emoji, a word from suggestions) |
//!
//! Pure file handling with no Android dependency, so it is tested on the desktop.

use std::path::Path;

/// A file the app reads is capped at this size (a document, a picture or a font).
pub const MAX_FILE_BYTES: u64 = 512 << 20;

/// One message from the Java side.
#[derive(Debug, PartialEq)]
pub enum Message {
    /// A file to open or insert (by its name's extension, as a dropped file).
    Opened { name: String, bytes: Vec<u8> },
    /// A picker closed.
    PickerClosed,
    /// The save with this ticket ended.
    Saved { ticket: u64, outcome: Outcome },
    /// Typed text.
    Text(String),
}

/// How a save ended.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    Saved,
    Cancelled,
    Failed(String),
}

/// Read and remove every complete message in `dir`, oldest first. A message that can't be read
/// is logged and removed, so it never blocks the ones after it.
pub fn drain(dir: &Path) -> Vec<Message> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut names: Vec<_> = entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e != "tmp")).collect();
    names.sort();
    let mut out = Vec::new();
    for path in names {
        let kind = path.extension().and_then(|e| e.to_str()).unwrap_or_default().to_string();
        let too_big = std::fs::metadata(&path).map(|m| m.len() > MAX_FILE_BYTES.saturating_add(4096)).unwrap_or(true);
        let parsed = if too_big {
            Err("too large".to_string())
        } else {
            std::fs::read(&path).map_err(|e| e.to_string()).and_then(|bytes| parse(&kind, bytes))
        };
        match parsed {
            Ok(Some(m)) => out.push(m),
            Ok(None) => {}
            Err(e) => log::warn!("inbox: skipping {}: {e}", path.display()),
        }
        if let Err(e) = std::fs::remove_file(&path) {
            log::warn!("inbox: cannot remove {}: {e}", path.display());
        }
    }
    out
}

/// One message from its kind (the file extension) and content. `Ok(None)` for a kind this
/// version doesn't know (a newer Java side), which is skipped.
pub fn parse(kind: &str, bytes: Vec<u8>) -> Result<Option<Message>, String> {
    match kind {
        "open" => {
            let split = bytes.iter().position(|&b| b == b'\n').ok_or("no name line")?;
            let name = String::from_utf8_lossy(bytes.get(..split).unwrap_or_default()).trim().to_string();
            let name = safe_name(&name);
            let data = bytes.get(split.saturating_add(1)..).unwrap_or_default().to_vec();
            Ok(Some(Message::Opened { name, bytes: data }))
        }
        "done" => Ok(Some(Message::PickerClosed)),
        "save" => {
            let text = String::from_utf8_lossy(&bytes);
            let mut lines = text.splitn(3, '\n');
            let ticket = lines.next().and_then(|t| t.trim().parse::<u64>().ok()).ok_or("no ticket")?;
            let outcome = match lines.next().map(str::trim) {
                Some("ok") => Outcome::Saved,
                Some("cancel") => Outcome::Cancelled,
                Some("fail") => Outcome::Failed(lines.next().map(str::trim).filter(|r| !r.is_empty()).unwrap_or("unknown error").to_string()),
                other => return Err(format!("unknown save outcome {other:?}")),
            };
            Ok(Some(Message::Saved { ticket, outcome }))
        }
        "text" => {
            let text = String::from_utf8_lossy(&bytes).to_string();
            Ok((!text.is_empty()).then_some(Message::Text(text)))
        }
        _ => Ok(None),
    }
}

/// A file name safe to show and to use as one path component: no folders, no control
/// characters, not empty, at most 200 characters.
pub fn safe_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or_default();
    let clean: String = base.chars().filter(|c| !c.is_control()).take(200).collect();
    let clean = clean.trim().trim_start_matches('.').to_string();
    if clean.is_empty() { "document".to_string() } else { clean }
}

/// The MIME type Android's save picker should offer for `name`.
pub fn mime_for(name: &str) -> &'static str {
    match name.rsplit('.').next().map(str::to_ascii_lowercase).as_deref() {
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("pdf") => "application/pdf",
        Some("docx") => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        Some("docm") => "application/vnd.ms-word.document.macroEnabled.12",
        Some("dotx") => "application/vnd.openxmlformats-officedocument.wordprocessingml.template",
        Some("doc") => "application/msword",
        Some("odt") => "application/vnd.oasis.opendocument.text",
        Some("rtf") => "application/rtf",
        Some("html" | "htm") => "text/html",
        Some("md") => "text/markdown",
        Some("txt") => "text/plain",
        Some("tex") => "application/x-tex",
        Some("json") => "application/json",
        Some("xml") => "application/xml",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opened_files_keep_their_bytes_and_lose_any_folder() {
        let mut bytes = b"../secret/Letter.docx\n".to_vec();
        bytes.extend([0u8, 10, 13, 255]);
        assert_eq!(parse("open", bytes), Ok(Some(Message::Opened { name: "Letter.docx".into(), bytes: vec![0, 10, 13, 255] })));
        assert!(parse("open", b"no newline".to_vec()).is_err());
    }

    #[test]
    fn save_outcomes() {
        assert_eq!(parse("save", b"7\nok".to_vec()), Ok(Some(Message::Saved { ticket: 7, outcome: Outcome::Saved })));
        assert_eq!(parse("save", b"8\ncancel\n".to_vec()), Ok(Some(Message::Saved { ticket: 8, outcome: Outcome::Cancelled })));
        assert_eq!(
            parse("save", b"9\nfail\ndisk full".to_vec()),
            Ok(Some(Message::Saved { ticket: 9, outcome: Outcome::Failed("disk full".into()) }))
        );
        assert!(parse("save", b"x\nok".to_vec()).is_err());
        assert!(parse("save", b"1\nmaybe".to_vec()).is_err());
    }

    #[test]
    fn unknown_kinds_and_empty_text_are_skipped() {
        assert_eq!(parse("future", vec![1, 2]), Ok(None));
        assert_eq!(parse("text", Vec::new()), Ok(None));
        assert_eq!(parse("text", "café ☕".as_bytes().to_vec()), Ok(Some(Message::Text("café ☕".into()))));
    }

    #[test]
    fn names_are_one_safe_component() {
        assert_eq!(safe_name("a/b\\c.docx"), "c.docx");
        assert_eq!(safe_name("..\u{0}"), "document");
        assert_eq!(safe_name(".hidden.txt"), "hidden.txt");
        assert_eq!(safe_name(&"x".repeat(500)).len(), 200);
    }

    #[test]
    fn drain_reads_in_order_skips_partial_files_and_cleans_up() {
        let dir = std::env::temp_dir().join(format!("wordcraft-android-inbox-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("0002.text"), "b").unwrap();
        std::fs::write(dir.join("0001.text"), "a").unwrap();
        std::fs::write(dir.join("0003.open.tmp"), "half written").unwrap();
        std::fs::write(dir.join("0004.save"), "nonsense").unwrap();
        let got = drain(&dir);
        assert_eq!(got, vec![Message::Text("a".into()), Message::Text("b".into())]);
        let left: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
        assert_eq!(left, vec!["0003.open.tmp".to_string()], "the bad message is gone, the partial one waits");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn mime_types() {
        assert_eq!(mime_for("Report.DOCX"), "application/vnd.openxmlformats-officedocument.wordprocessingml.document");
        assert_eq!(mime_for("out.pdf"), "application/pdf");
        assert_eq!(mime_for("noext"), "application/octet-stream");
    }
}
