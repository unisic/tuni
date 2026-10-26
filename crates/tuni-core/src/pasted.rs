//! Images pasted into a terminal, kept as files a program can be given.
//!
//! A screenshot copied to the clipboard is an image and no text, and a shell
//! has no way to take one: the paste would send nothing. A coding agent takes
//! a path to an image as the image, the way dropping the file on the pane
//! gives it one, so the paste writes the image to a file of its own and types
//! the path instead. Kero does the same.

use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The image types a paste is written out for, by MIME type, with the
/// extension each is saved under. The ones coding agents take.
pub const TYPES: &[(&str, &str)] = &[
    ("image/png", "png"),
    ("image/jpeg", "jpg"),
    ("image/gif", "gif"),
    ("image/webp", "webp"),
];

/// How long a pasted image's file is kept. An agent may come back to the path
/// long after the paste, and a week of screenshots is still a small directory.
const KEEP: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// Where pasted images go: under the cache, since every one of them is a copy
/// of something the clipboard had.
#[must_use]
pub fn directory() -> PathBuf {
    crate::settings::cache_dir().join("pasted")
}

/// Writes an image to a new file in `directory`, which only this user can
/// enter, and hands back its path. Pastes older than a week go first.
///
/// The name is never reused: the file is created new or not at all, so a
/// second paste in the same instant fails rather than writing over the first.
/// A failed write takes its half-written file with it.
pub fn save(directory: &Path, image: &[u8], extension: &str) -> io::Result<PathBuf> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(directory)?;
    prune(directory, SystemTime::now() - KEEP);

    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let path = directory.join(format!("paste-{stamp}-{}.{extension}", std::process::id()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        // A screenshot is whatever was on screen, which is nobody else's.
        .mode(0o600)
        .open(&path)?;
    if let Err(error) = file.write_all(image) {
        drop(file);
        let _ = fs::remove_file(&path);
        return Err(error);
    }
    Ok(path)
}

/// Removes the pastes last written before `before`, and nothing else that
/// happens to share the directory.
fn prune(directory: &Path, before: SystemTime) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let ours = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("paste-"))
            && path
                .extension()
                .and_then(|extension| extension.to_str())
                .is_some_and(|extension| TYPES.iter().any(|&(_, known)| known == extension));
        let old = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .is_ok_and(|modified| modified < before);
        if ours && old {
            let _ = fs::remove_file(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn a_pasted_image_is_kept_privately_and_a_week_old_one_goes() {
        let directory = std::env::temp_dir().join(format!("tuni-pasted-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);

        let first = save(&directory, b"\x89PNG", "png").expect("first paste");
        assert_eq!(fs::read(&first).expect("read back"), b"\x89PNG");
        assert_eq!(first.extension().and_then(|e| e.to_str()), Some("png"));
        let mode = |path: &Path| fs::metadata(path).expect("metadata").permissions().mode() & 0o777;
        assert_eq!(mode(&directory), 0o700);
        assert_eq!(mode(&first), 0o600);

        // Aged past the week, beside a file the directory merely holds.
        let stale = SystemTime::now() - KEEP - Duration::from_secs(60);
        fs::File::options()
            .write(true)
            .open(&first)
            .and_then(|file| file.set_modified(stale))
            .expect("age the paste");
        let stranger = directory.join("notes.png");
        fs::write(&stranger, b"mine").expect("stranger");
        fs::File::options()
            .write(true)
            .open(&stranger)
            .and_then(|file| file.set_modified(stale))
            .expect("age the stranger");

        let second = save(&directory, b"GIF89a", "gif").expect("second paste");
        assert_ne!(first, second);
        assert!(!first.exists(), "a week-old paste is removed");
        assert!(second.exists());
        assert!(
            stranger.exists(),
            "a file that is not a paste is left alone"
        );

        let _ = fs::remove_dir_all(&directory);
    }
}
