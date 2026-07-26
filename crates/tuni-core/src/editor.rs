//! Opening a file, and writing it back.
//!
//! What the editor is allowed to open, and how a save reaches the disk without
//! a crash halfway through costing the file. The widget above this decides how
//! it looks; this decides what is in it.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// The largest file the editor will read. kero's own limit, and for the same
/// reason: past it the text view stops being an editor and starts being a way
/// to hang the window.
pub const MAX_BYTES: u64 = 5 << 20;

/// Extensions the editor hands to the image viewer instead of the text view.
/// Matched case-insensitively.
const IMAGE_EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "tiff", "tif", "bmp", "ico", "avif", "svg",
];

/// What came back from the disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Document {
    /// Text, exactly as it was on disk — line endings and a missing final
    /// newline included, so saving an untouched file writes back what it read.
    Text(String),
    /// Something the image viewer should draw. The bytes stay on disk; GTK
    /// reads them itself.
    Image,
    /// Readable, but not as text.
    Binary,
    /// Larger than [`MAX_BYTES`], in bytes.
    TooLarge(u64),
    /// The disk said no. Carries what it said.
    Unreadable(String),
}

impl Document {
    /// Whether this is something the user can type into.
    #[must_use]
    pub fn is_editable(&self) -> bool {
        matches!(self, Self::Text(_))
    }

    /// The line to show when there is nothing to edit.
    #[must_use]
    pub fn message(&self) -> Option<String> {
        match self {
            Self::Text(_) | Self::Image => None,
            Self::Binary => Some("This is not a text file.".to_owned()),
            Self::TooLarge(size) => Some(format!(
                "This file is {}, past the {} the editor will open.",
                size_text(*size),
                size_text(MAX_BYTES)
            )),
            Self::Unreadable(reason) => Some(reason.clone()),
        }
    }
}

/// Reads a file the editor was asked for.
#[must_use]
pub fn load(path: &Path) -> Document {
    if is_image(path) {
        return Document::Image;
    }
    let size = match fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => {
            return Document::Unreadable("This is a directory.".to_owned());
        }
        Ok(metadata) => metadata.len(),
        Err(error) => return Document::Unreadable(error.to_string()),
    };
    if size > MAX_BYTES {
        return Document::TooLarge(size);
    }
    match fs::read(path) {
        // Not `read_to_string`: a file that turns out to be binary is a
        // different answer, not a failure, and this way the error message is
        // only ever a real one from the disk.
        Ok(bytes) => match String::from_utf8(bytes) {
            Ok(text) => Document::Text(text),
            Err(_) => Document::Binary,
        },
        Err(error) => Document::Unreadable(error.to_string()),
    }
}

#[must_use]
pub fn is_image(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            let extension = extension.to_ascii_lowercase();
            IMAGE_EXTENSIONS.contains(&extension.as_str())
        })
}

/// Writes the file, whole, without a window where it is half of each version.
///
/// The text goes to a neighbour of the target and is renamed onto it, which is
/// atomic on every filesystem Linux ships: an interrupted save costs the new
/// text, never the old. Two things follow from renaming rather than truncating,
/// and both are handled here — a symlink would be replaced by a regular file,
/// so it is resolved first, and the new file would be created with the umask's
/// permissions, so the old file's are copied onto it. A file with more than one
/// hard link keeps only this path pointing at the new content; git's own
/// checkout behaves the same way.
pub fn save(path: &Path, text: &str) -> Result<(), String> {
    let target = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let Some(directory) = target.parent() else {
        return Err("That path has no directory to write into.".to_owned());
    };
    let temporary = directory.join(temporary_name(&target));

    let write = || -> std::io::Result<()> {
        let mut file = fs::File::create(&temporary)?;
        file.write_all(text.as_bytes())?;
        // Before the rename, so a machine that loses power just after it has
        // the bytes to go with the name.
        file.sync_all()?;
        drop(file);
        if let Ok(metadata) = fs::metadata(&target) {
            fs::set_permissions(&temporary, metadata.permissions())?;
        }
        fs::rename(&temporary, &target)
    };

    write().map_err(|error| {
        let _ = fs::remove_file(&temporary);
        error.to_string()
    })
}

/// A name beside the file being written. The process id is in it because two
/// windows saving the same file at once would otherwise share the temporary and
/// write each other's halves.
fn temporary_name(target: &Path) -> PathBuf {
    let name = target
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("file");
    PathBuf::from(format!(".{name}.tuni-{}", std::process::id()))
}

fn size_text(bytes: u64) -> String {
    const MIB: f64 = 1024.0 * 1024.0;
    const KIB: f64 = 1024.0;
    #[expect(
        clippy::cast_precision_loss,
        reason = "sizes this large are being rounded to one decimal anyway"
    )]
    let bytes = bytes as f64;
    if bytes >= MIB {
        format!("{:.1} MiB", bytes / MIB)
    } else if bytes >= KIB {
        format!("{:.1} KiB", bytes / KIB)
    } else {
        format!("{bytes:.0} bytes")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A directory of its own, removed when the test ends.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("tuni-editor-{name}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).expect("scratch directory");
            Self(path)
        }

        fn file(&self, name: &str, contents: &str) -> PathBuf {
            let path = self.0.join(name);
            fs::write(&path, contents).expect("write");
            path
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn text_comes_back_byte_for_byte() {
        let scratch = Scratch::new("text");
        let path = scratch.file("notes.txt", "one\r\ntwo\nthree");
        assert_eq!(
            load(&path),
            Document::Text("one\r\ntwo\nthree".to_owned()),
            "line endings and the missing final newline are the file's, not ours"
        );
    }

    #[test]
    fn a_file_of_bytes_is_not_a_file_of_text() {
        let scratch = Scratch::new("binary");
        let path = scratch.0.join("blob.dat");
        fs::write(&path, [0xff, 0xfe, 0x00, 0x01]).expect("write");
        assert_eq!(load(&path), Document::Binary);
    }

    #[test]
    fn an_image_is_left_to_the_image_viewer() {
        assert_eq!(
            load(Path::new("/does/not/exist/photo.PNG")),
            Document::Image
        );
        assert!(is_image(Path::new("a/b/c.jpeg")));
        assert!(!is_image(Path::new("a/b/c.rs")));
        assert!(!is_image(Path::new("png")));
    }

    #[test]
    fn a_file_past_the_limit_is_refused_before_it_is_read() {
        let scratch = Scratch::new("large");
        let path = scratch.file("big.txt", "");
        let file = fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("open");
        file.set_len(MAX_BYTES + 1).expect("grow");
        drop(file);
        let Document::TooLarge(size) = load(&path) else {
            panic!("a file over the limit should be refused");
        };
        assert_eq!(size, MAX_BYTES + 1);
    }

    #[test]
    fn a_missing_file_says_what_the_disk_said() {
        let Document::Unreadable(reason) = load(Path::new("/does/not/exist/notes.txt")) else {
            panic!("a missing file is unreadable");
        };
        assert!(!reason.is_empty());
    }

    #[test]
    fn a_directory_is_not_a_document() {
        let scratch = Scratch::new("directory");
        assert!(matches!(load(&scratch.0), Document::Unreadable(_)));
    }

    #[test]
    fn saving_replaces_the_contents_and_leaves_nothing_beside_them() {
        let scratch = Scratch::new("save");
        let path = scratch.file("notes.txt", "before");
        save(&path, "after").expect("save");
        assert_eq!(fs::read_to_string(&path).expect("read"), "after");
        let left: Vec<_> = fs::read_dir(&scratch.0)
            .expect("read directory")
            .filter_map(Result::ok)
            .map(|entry| entry.file_name())
            .collect();
        assert_eq!(left.len(), 1, "the temporary is renamed, not left behind");
    }

    #[test]
    fn saving_keeps_the_permissions_the_file_had() {
        use std::os::unix::fs::PermissionsExt;

        let scratch = Scratch::new("mode");
        let path = scratch.file("run.sh", "#!/bin/sh\n");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod");
        save(&path, "#!/bin/sh\necho hello\n").expect("save");
        let mode = fs::metadata(&path).expect("stat").permissions().mode();
        assert_eq!(mode & 0o777, 0o755, "an executable script stays executable");
    }

    #[test]
    fn saving_through_a_symlink_writes_the_file_it_points_at() {
        let scratch = Scratch::new("symlink");
        let target = scratch.file("real.txt", "before");
        let link = scratch.0.join("link.txt");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");

        save(&link, "after").expect("save");

        assert_eq!(fs::read_to_string(&target).expect("read"), "after");
        assert!(
            fs::symlink_metadata(&link)
                .expect("stat")
                .file_type()
                .is_symlink(),
            "the link is still a link"
        );
    }

    #[test]
    fn saving_where_it_cannot_be_written_says_so() {
        let error = save(Path::new("/does/not/exist/notes.txt"), "text")
            .expect_err("a missing directory cannot be written to");
        assert!(!error.is_empty());
    }
}
