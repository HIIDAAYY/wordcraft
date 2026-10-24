//! When a save may write over a file without asking.
//!
//! Android gives the app a file's contents and, at best, lasting permission to write back to it;
//! never a path. The activity remembers which file each name was opened from or saved to. Writing
//! back by name is only safe for a plain Save of a document that has such a file: a new document
//! named "Document1" must never land on some other "Document1.docx" saved earlier.
//!
//! Pure logic, tested on the desktop.

/// What the save hooks know between a save starting and the activity's answer.
#[derive(Default)]
pub struct Saving {
    /// The command the next `download` is for (`file.save`, `file.saveAs`, `file.exportPdf`…).
    pub command: String,
    /// Whether the document has a file: one it was opened from or saved to. Kept up to date by
    /// the shell every frame.
    pub has_file: bool,
    /// The command each save in flight was started for, by ticket.
    pub commands: std::collections::HashMap<u64, String>,
}

impl Saving {
    /// Only a plain Save of a document that has a file writes back to that file without asking.
    /// A new document, Save As and every export go to the save picker, so a file is never
    /// overwritten because a new document happens to have the same name.
    pub fn writes_back(&self) -> bool {
        self.command == "file.save" && self.has_file
    }
}

#[cfg(test)]
mod tests {
    use super::Saving;

    fn saving(command: &str, has_file: bool) -> Saving {
        Saving { command: command.into(), has_file, ..Default::default() }
    }

    #[test]
    fn only_a_plain_save_of_a_document_with_a_file_writes_back() {
        assert!(saving("file.save", true).writes_back());
        assert!(!saving("file.save", false).writes_back(), "a new document asks where, whatever its name");
        assert!(!saving("file.saveAs", true).writes_back(), "Save As always asks where");
        assert!(!saving("file.exportPdf", true).writes_back());
        assert!(!saving("", true).writes_back(), "a save the app didn't name the command of asks");
    }
}
