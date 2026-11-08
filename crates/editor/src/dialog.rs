//! A minimal in-app file browser.
//!
//! `Open...` and `SaveAs...` used to print "pass a path on the command line",
//! which makes the menu entries pointless: a new map could never be saved at
//! all. This module provides a directory browser built from `std::fs` and egui,
//! so no new crate is needed (AGENTS.md 15.1 forbids a file-dialog dependency).
//!
//! The dialog is a pure state machine over a directory listing, so the
//! navigation logic is unit-testable without a window.

use std::path::{Path, PathBuf};

/// What the dialog is being used for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogMode {
    /// Pick an existing file to open.
    Open,
    /// Choose a path to write.
    SaveAs,
}

/// A directory entry shown in the browser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Display name (file or directory name).
    pub name: String,
    /// Whether this entry is a directory.
    pub is_dir: bool,
}

/// The file-browser dialog state.
pub struct FileDialog {
    mode: DialogMode,
    dir: PathBuf,
    entries: Vec<Entry>,
    selected: Option<PathBuf>,
    filename: String,
    filter_ext: &'static str,
    error: Option<String>,
    confirmed: Option<PathBuf>,
}

impl FileDialog {
    /// Opens the dialog in `dir`, listing files ending with `filter_ext`.
    pub fn new(mode: DialogMode, dir: impl Into<PathBuf>, filter_ext: &'static str) -> Self {
        let dir = dir.into();
        let mut d = FileDialog {
            mode,
            dir,
            entries: Vec::new(),
            selected: None,
            filename: String::new(),
            filter_ext,
            error: None,
            confirmed: None,
        };
        d.refresh();
        d
    }

    /// The mode.
    pub fn mode(&self) -> DialogMode {
        self.mode
    }

    /// The directory currently listed.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The entries in the current directory (directories first, then files).
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// The selected file, if one was clicked.
    pub fn selected(&self) -> Option<&Path> {
        self.selected.as_deref()
    }

    /// The file-name field (used by Save As).
    pub fn filename(&self) -> &str {
        &self.filename
    }

    /// Mutable file-name field.
    pub fn filename_mut(&mut self) -> &mut String {
        &mut self.filename
    }

    /// The last error, if a navigation step failed.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Re-reads the current directory.
    pub fn refresh(&mut self) {
        self.entries.clear();
        match std::fs::read_dir(&self.dir) {
            Ok(iter) => {
                for entry in iter.flatten() {
                    let path = entry.path();
                    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                        continue;
                    };
                    let is_dir = path.is_dir();
                    if !is_dir && !self.matches_filter(&path) {
                        continue;
                    }
                    self.entries.push(Entry {
                        name: name.to_string(),
                        is_dir,
                    });
                }
                // Directories first, then case-insensitive name order.
                self.entries.sort_by(|a, b| {
                    b.is_dir
                        .cmp(&a.is_dir)
                        .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                });
                self.error = None;
            }
            Err(e) => {
                self.error = Some(format!("cannot read {}: {e}", self.dir.display()));
            }
        }
    }

    /// Whether a file passes the extension filter.
    fn matches_filter(&self, path: &Path) -> bool {
        self.filter_ext.is_empty()
            || path
                .extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case(self.filter_ext))
    }

    /// Enters a subdirectory.
    pub fn enter(&mut self, name: &str) {
        self.dir = self.dir.join(name);
        self.selected = None;
        self.refresh();
    }

    /// Goes to the parent directory, stopping at the filesystem root.
    pub fn up(&mut self) {
        if let Some(parent) = self.dir.parent() {
            self.dir = parent.to_path_buf();
            self.selected = None;
            self.refresh();
        }
    }

    /// Selects a file entry.
    pub fn select(&mut self, name: &str) {
        let path = self.dir.join(name);
        self.filename = name.to_string();
        self.selected = Some(path);
    }

    /// The path that would be produced right now, if valid.
    pub fn pending_path(&self) -> Option<PathBuf> {
        match self.mode {
            DialogMode::Open => self.selected.clone(),
            DialogMode::SaveAs => {
                let name = self.filename.trim();
                if name.is_empty() {
                    return None;
                }
                let mut path = self.dir.join(name);
                if !name
                    .to_ascii_lowercase()
                    .ends_with(&format!(".{}", self.filter_ext))
                {
                    path.set_extension(self.filter_ext);
                }
                Some(path)
            }
        }
    }

    /// Confirms the dialog, yielding the chosen path.
    ///
    /// Returns `None` when nothing valid is selected; the caller keeps the
    /// dialog open so the user can fix it.
    pub fn confirm(&mut self) -> Option<PathBuf> {
        let path = self.pending_path()?;
        if self.mode == DialogMode::Open && !path.is_file() {
            self.error = Some(format!("not a file: {}", path.display()));
            return None;
        }
        self.confirmed = Some(path.clone());
        Some(path)
    }

    /// Whether the dialog has been confirmed.
    pub fn confirmed(&self) -> Option<&Path> {
        self.confirmed.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("releste-dialog-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn lists_only_matching_files_and_directories() {
        let dir = temp_dir("listing");
        std::fs::write(dir.join("a.map"), "x").unwrap();
        std::fs::write(dir.join("b.txt"), "x").unwrap();
        std::fs::create_dir_all(dir.join("sub")).unwrap();

        let d = FileDialog::new(DialogMode::Open, &dir, "map");
        let names: Vec<&str> = d.entries().iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["sub", "a.map"],
            "dirs first, then filtered files"
        );
    }

    #[test]
    fn navigation_up_and_down_works() {
        let dir = temp_dir("nav");
        std::fs::create_dir_all(dir.join("inner")).unwrap();
        std::fs::write(dir.join("inner").join("x.map"), "x").unwrap();

        let mut d = FileDialog::new(DialogMode::Open, &dir, "map");
        d.enter("inner");
        assert_eq!(d.entries().len(), 1);
        d.select("x.map");
        assert_eq!(d.pending_path().unwrap(), dir.join("inner").join("x.map"));

        d.up();
        assert_eq!(d.dir(), dir.as_path());
    }

    #[test]
    fn save_as_appends_the_extension() {
        let dir = temp_dir("saveas");
        let mut d = FileDialog::new(DialogMode::SaveAs, &dir, "map");
        d.filename_mut().push_str("my_new_map");
        assert_eq!(d.pending_path().unwrap(), dir.join("my_new_map.map"));

        // An explicit extension is not doubled.
        *d.filename_mut() = "other.map".to_string();
        assert_eq!(d.pending_path().unwrap(), dir.join("other.map"));
    }

    #[test]
    fn open_rejects_a_non_file_target() {
        let dir = temp_dir("openmissing");
        let mut d = FileDialog::new(DialogMode::Open, &dir, "map");
        d.select("ghost.map");
        assert!(d.confirm().is_none(), "must not confirm a missing file");
        assert!(d.error().is_some());
    }

    #[test]
    fn save_as_can_confirm_a_new_file() {
        let dir = temp_dir("savenew");
        let mut d = FileDialog::new(DialogMode::SaveAs, &dir, "map");
        *d.filename_mut() = "fresh".to_string();
        let path = d.confirm().expect("save-as must accept a new name");
        assert_eq!(path, dir.join("fresh.map"));
        assert_eq!(d.confirmed(), Some(path.as_path()));
    }

    #[test]
    fn a_missing_directory_reports_an_error_instead_of_panicking() {
        let d = FileDialog::new(DialogMode::Open, "/nonexistent/releste/dir", "map");
        assert!(d.error().is_some());
        assert!(d.entries().is_empty());
    }
}
