//! Where a document is, in words the user knows, for the Recent list.
//!
//! Android hands the app a file's contents, never a path: a document opened or saved here is
//! known by its name, and the activity keeps the `content://` address each name was last opened
//! from or saved to (while the app may still write there). It lists them in
//! `known-locations.txt` (one `name<TAB>address` per line), which this reads: Recent keeps the
//! names it lists, shows the folder each address names, and reopens them through the activity.
//!
//! Pure data handling, tested on the desktop.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// The names the activity knows an address for, as last read.
#[derive(Default)]
pub struct Known {
    file: PathBuf,
    /// The list file's modification time when last read.
    read_at: Option<SystemTime>,
    by_name: HashMap<String, String>,
}

/// Longest list file read (a few hundred names at most in practice).
const MAX_LIST_BYTES: u64 = 1 << 20;

impl Known {
    pub fn new(file: PathBuf) -> Self {
        let mut known = Self { file, read_at: None, by_name: HashMap::new() };
        known.refresh();
        known
    }

    /// Read the list again when the activity changed it.
    pub fn refresh(&mut self) {
        let meta = std::fs::metadata(&self.file).ok();
        let modified = meta.as_ref().and_then(|m| m.modified().ok());
        if modified.is_some() && modified == self.read_at {
            return;
        }
        self.read_at = modified;
        self.by_name = match meta {
            Some(m) if m.len() <= MAX_LIST_BYTES => std::fs::read_to_string(&self.file).map(|t| parse(&t)).unwrap_or_default(),
            _ => HashMap::new(),
        };
    }

    /// The address the document called `name` was last opened from or saved to.
    pub fn address(&self, name: &str) -> Option<&str> {
        self.by_name.get(name).map(String::as_str)
    }

    /// (name, folder) for a Recent entry the activity knows.
    pub fn place(&self, name: &str) -> Option<(String, String)> {
        self.address(name).map(|uri| (name.to_string(), folder_of(uri)))
    }

    pub fn file(&self) -> &Path {
        &self.file
    }
}

/// `name<TAB>address` lines; anything else is skipped.
fn parse(text: &str) -> HashMap<String, String> {
    text.lines()
        .filter_map(|l| l.split_once('\t'))
        .filter(|(name, uri)| !name.is_empty() && uri.starts_with("content://"))
        .map(|(name, uri)| (name.to_string(), uri.to_string()))
        .collect()
}

/// The folder a `content://` address names, as the user sees it in the file picker:
/// `…/document/primary%3ADownload%2FReports%2Fq3.xlsx` → `Download/Reports`. Providers whose
/// addresses carry no path are named after the place they stand for, or nothing.
pub fn folder_of(uri: &str) -> String {
    let Some(rest) = uri.strip_prefix("content://") else { return String::new() };
    let authority = rest.split('/').next().unwrap_or("");
    match authority {
        "com.android.providers.downloads.documents" => return "Download".into(),
        "com.google.android.apps.docs.storage" => return "Google Drive".into(),
        _ => {}
    }
    if authority != "com.android.externalstorage.documents" {
        return String::new();
    }
    let Some(document) = rest.rsplit("/document/").next().filter(|d| !d.is_empty() && *d != rest) else { return String::new() };
    let decoded = percent_decode(document);
    // "primary:Download/x.xlsx" (the device), "1A2B-3C4D:Download/x.xlsx" (an SD card).
    let (volume, path) = decoded.split_once(':').unwrap_or(("", decoded.as_str()));
    let folder = path.rsplit_once('/').map_or("", |(f, _)| f);
    match volume {
        "primary" | "" => folder.to_string(),
        _ if folder.is_empty() => "SD card".into(),
        _ => format!("SD card/{folder}"),
    }
}

/// `%XX` escapes decoded (malformed ones kept as they are); not UTF-8: replaced characters.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while let Some(&b) = bytes.get(i) {
        if b == b'%'
            && let (Some(h), Some(l)) = (bytes.get(i + 1).and_then(|c| hex(*c)), bytes.get(i + 2).and_then(|c| hex(*c)))
        {
            out.push(h * 16 + l);
            i += 3;
        } else {
            out.push(b);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_read_from_storage_addresses() {
        let device = "content://com.android.externalstorage.documents/document/primary%3ADownload%2FGridCraft-tes%2Fpenjualan.xlsx";
        assert_eq!(folder_of(device), "Download/GridCraft-tes");
        let tree = "content://com.android.externalstorage.documents/tree/primary%3ADownload/document/primary%3ADownload%2Fq3.xlsx";
        assert_eq!(folder_of(tree), "Download");
        assert_eq!(folder_of("content://com.android.externalstorage.documents/document/primary%3Aq3.xlsx"), "");
        assert_eq!(folder_of("content://com.android.externalstorage.documents/document/1A2B-3C4D%3AWork%2Fq3.xlsx"), "SD card/Work");
        assert_eq!(folder_of("content://com.android.providers.downloads.documents/document/msf%3A1000001234"), "Download");
        assert_eq!(folder_of("content://com.google.android.apps.docs.storage/document/acc%3D1%3Bdoc%3D9"), "Google Drive");
        assert_eq!(folder_of("content://media/external/file/12"), "");
        assert_eq!(folder_of("/data/user/0/x/files/docs/1/q3.xlsx"), "");
        // Hostile escapes never panic.
        assert_eq!(folder_of("content://com.android.externalstorage.documents/document/primary%3AA%2%ZZ%E2%82%2Fb.xlsx"), "A%2%ZZ\u{fffd}");
    }

    #[test]
    fn the_list_is_read_again_when_it_changes() {
        let dir = std::env::temp_dir().join(format!("wordcraft-known-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("known-locations.txt");
        let _ = std::fs::remove_file(&file);
        let mut known = Known::new(file.clone());
        assert_eq!(known.address("a.docx"), None, "no list yet");
        let uri = "content://com.android.externalstorage.documents/document/primary%3ADownload%2Fa.docx";
        std::fs::write(&file, format!("a.docx\t{uri}\nbroken line\n\tcontent://x\nb.docx\tfile:///b.docx\n")).unwrap();
        known.read_at = None;
        known.refresh();
        assert_eq!(known.address("a.docx"), Some(uri));
        assert_eq!(known.place("a.docx"), Some(("a.docx".into(), "Download".into())));
        assert_eq!(known.address("b.docx"), None, "only content addresses");
        assert_eq!(known.by_name.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
