//! Whether a newer Tuni has been released.
//!
//! No distribution packages Tuni yet, so nothing on the machine notices a
//! release: an installed copy stays the version it was installed at until
//! somebody goes looking. This asks the GitHub release page what the newest
//! version is, and `scripts/install.sh` is what installs the answer.
//!
//! `curl` rather than an HTTP client crate, for the same reason git is `git`:
//! one question a day about a public JSON document does not justify a TLS
//! stack, a certificate store and an async runtime in a terminal's address
//! space, and the machine already has a tool that asks it correctly. The
//! process runs behind [`gio::spawn_blocking`] on the widget side, since it
//! talks to the network.

use std::process::Command;

/// The release page a person is sent to when the button cannot do the work.
pub const RELEASES_URL: &str = "https://github.com/unisic/tuni/releases";

/// The canonical installer, fetched fresh every time rather than remembered:
/// an installed copy of the script is exactly as old as the Tuni it came with,
/// and the one thing an update must not depend on is the version being
/// replaced.
const INSTALLER_URL: &str = "https://raw.githubusercontent.com/unisic/tuni/main/scripts/install.sh";

const RELEASES_API: &str = "https://api.github.com/repos/unisic/tuni/releases/latest";

/// Where the newest version is asked about. `TUNI_UPDATE_API` overrides it, so
/// a release can be faked with a `file://` URL while the dialog is being
/// worked on; `scripts/install.sh` reads the same variable, and the pane the
/// installer runs in inherits it.
#[must_use]
pub fn api_url() -> String {
    std::env::var("TUNI_UPDATE_API").unwrap_or_else(|_| RELEASES_API.to_owned())
}

/// The newest released version, or nothing when the question could not be
/// asked. Blocking: a network round trip and a process.
///
/// Nothing distinguishes "GitHub said no" from "there is no curl here", and
/// nothing needs to: neither is an answer, and a terminal that cannot check
/// for updates has not gone wrong.
#[must_use]
pub fn latest() -> Option<String> {
    let output = Command::new("curl")
        .args([
            "-fsSL",
            "--max-time",
            "10",
            "-H",
            "Accept: application/vnd.github+json",
            &api_url(),
        ])
        .env("LC_ALL", "C")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_tag(&String::from_utf8_lossy(&output.stdout))
}

/// The version out of a release document, with the `v` a tag carries stripped.
#[must_use]
pub fn parse_tag(json: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    let tag = value.get("tag_name")?.as_str()?.trim();
    let version = tag.strip_prefix('v').unwrap_or(tag);
    (!version.is_empty()).then(|| version.to_owned())
}

/// Whether `latest` is a later version than `current`.
///
/// Compared as numbers per component, because 1.10 is after 1.9 and a string
/// compare says the opposite. A component that is not a number stops the
/// comparison there and the two count as equal: a tag nobody planned for is
/// not a reason to announce an update.
#[must_use]
pub fn is_newer(latest: &str, current: &str) -> bool {
    let parts = |version: &str| -> Vec<u64> {
        version
            .split('.')
            .map(str::trim)
            .map_while(|part| part.parse::<u64>().ok())
            .collect()
    };
    let (latest, current) = (parts(latest), parts(current));
    if latest.is_empty() || current.is_empty() {
        return false;
    }
    // Zero-extended, so 1.3 and 1.3.0 are the same version rather than one
    // being shorter than the other.
    let width = latest.len().max(current.len());
    let at = |version: &[u64], index: usize| version.get(index).copied().unwrap_or(0);
    (0..width)
        .find_map(|index| match at(&latest, index).cmp(&at(&current, index)) {
            std::cmp::Ordering::Equal => None,
            other => Some(other == std::cmp::Ordering::Greater),
        })
        .unwrap_or(false)
}

/// The line the Update button runs in a terminal of its own.
///
/// Piped into `bash` rather than downloaded and executed, because the download
/// is the point: whatever the installer has learned since this Tuni was built
/// is what installs it. It runs in a pane, not behind the window, for the
/// reason every other privileged thing here does — `sudo` asks for a password
/// and a pane is the one place in this program that can answer.
#[must_use]
pub fn install_command() -> String {
    format!("curl -fsSL {INSTALLER_URL} | bash -s -- -y")
}

/// Whether this process is inside a Flatpak sandbox, where the installer
/// cannot reach the host's package manager and there is nothing to offer.
#[must_use]
pub fn in_flatpak() -> bool {
    std::path::Path::new("/.flatpak-info").exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tag_loses_the_v_it_was_written_with() {
        assert_eq!(
            parse_tag(r#"{"tag_name": "v1.3.0", "name": "Tuni 1.3.0"}"#).as_deref(),
            Some("1.3.0")
        );
        assert_eq!(parse_tag(r#"{"tag_name": "1.3.0"}"#).as_deref(), Some("1.3.0"));
    }

    #[test]
    fn a_document_without_a_tag_is_no_answer() {
        assert_eq!(parse_tag("{}"), None);
        assert_eq!(parse_tag(r#"{"tag_name": ""}"#), None);
        assert_eq!(parse_tag("<html>rate limited</html>"), None);
    }

    #[test]
    fn ten_comes_after_nine() {
        assert!(is_newer("1.10.0", "1.9.0"));
        assert!(!is_newer("1.9.0", "1.10.0"));
    }

    #[test]
    fn the_version_that_is_installed_is_not_an_update() {
        assert!(!is_newer("1.3.0", "1.3.0"));
        assert!(!is_newer("1.3", "1.3.0"));
        assert!(!is_newer("1.2.9", "1.3.0"));
    }

    #[test]
    fn a_version_nobody_planned_for_announces_nothing() {
        assert!(!is_newer("nightly", "1.3.0"));
        assert!(!is_newer("1.3.0", ""));
    }

    #[test]
    fn the_install_command_is_one_shell_line() {
        let command = install_command();
        assert!(!command.contains('\n'));
        assert!(command.contains("install.sh"));
    }
}
