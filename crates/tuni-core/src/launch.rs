//! What a launch decides before there is a window: whether it joins the Tuni
//! already running, which directory it was about, and the environment the
//! process and its first shell start with.
//!
//! Here rather than in the application because two programs ask. `tuni` is a
//! launcher linked against GLib alone, which starts the first window's shell
//! and then becomes `tuni-gtk`; `tuni-gtk` is the application, and run by
//! itself it has to arrive at the same answers.

use std::ffi::OsString;
use std::path::PathBuf;

/// The application id: the bus name a running Tuni owns, the desktop entry and
/// the icon.
pub const APP_ID: &str = "dev.unisic.Tuni";

/// Whether this run must not join the one already going.
///
/// A capture renders a window and quits, and forwarded to a running instance it
/// would render nothing and quit anyway. A build being worked on is the same
/// problem the other way around: `cargo run` while an installed Tuni is up
/// would open a window in the installed one and test nothing.
#[must_use]
pub fn separate_process() -> bool {
    std::env::var_os("TUNI_CAPTURE_PNG").is_some()
        || std::env::var("TUNI_SINGLE_INSTANCE").is_ok_and(|value| value == "0")
}

/// The directory a launch was about, if it was about one.
///
/// An argument names it outright, which is what a shell alias or a `.desktop`
/// file with a path in it would use. Otherwise it is the working directory the
/// launch was made from, which is how a file manager says it: KDE's launcher
/// sets the child process's directory rather than passing a flag, since only
/// konsole takes one. A launch from the home directory is not read as naming
/// it, because that is where an application menu launches everything from and a
/// session restored with a project per checkout does not want a tenth project
/// for the home directory on top.
#[must_use]
pub fn launch_directory(
    cwd: Option<PathBuf>,
    arguments: &[OsString],
    home: Option<PathBuf>,
) -> Option<PathBuf> {
    let named = arguments
        .iter()
        .skip(1)
        .find(|argument| !argument.to_string_lossy().starts_with('-'));
    if let Some(named) = named {
        let path = PathBuf::from(named);
        let path = match path.is_absolute() {
            true => path,
            false => cwd?.join(path),
        };
        return path.is_dir().then_some(path);
    }

    let cwd = cwd?;
    (Some(&cwd) != home.as_ref() && cwd.is_dir()).then_some(cwd)
}

/// Silences Mesa's RADV conformance notice, and turns GSK's damage tracking
/// off for a translucent window. Both through the environment, so the first
/// shell, started before GTK, inherits the same environment the later ones
/// do. A value already set is left alone, which also makes a second call, by
/// the program the launcher becomes, cost nothing but the settings read.
///
/// RADV prints its notice for every Vulkan context, which is what GTK renders
/// with. It is a statement about the driver's certification, not about this
/// machine or this program, and Mesa has an environment variable for saying so
/// once and not again. Somebody who asked for the notice can have it.
///
/// A translucent window has no opaque chrome: every bar's background is
/// `transparent` and the only paint under a label is the window's own color,
/// so a pixel GSK's damage tracking leaves out is never covered again and a
/// sliver of text that has since changed stays on screen. Full redraws are the
/// one lever GSK offers against that, and the window is already repainting
/// whole for every terminal frame, so the added work is the chrome around it.
/// An opaque window paints its own backgrounds and is left with damage
/// tracking on, because it does not have the problem. `TUNI_PARTIAL_REDRAW=1`
/// puts it back either way.
///
/// # Safety
///
/// Writes the environment, so no other thread may be running: call it before
/// GTK, its renderer, or a thread of the caller's own exists.
pub unsafe fn quieten(background_opacity: f64) {
    if std::env::var_os("MESA_VK_IGNORE_CONFORMANCE_WARNING").is_none() {
        // SAFETY: the caller's promise that nothing else is running.
        unsafe { std::env::set_var("MESA_VK_IGNORE_CONFORMANCE_WARNING", "1") };
    }
    if std::env::var_os("GSK_DEBUG").is_none()
        && std::env::var_os("TUNI_PARTIAL_REDRAW").is_none()
        && background_opacity < 1.0
    {
        // SAFETY: as above.
        unsafe { std::env::set_var("GSK_DEBUG", "full-redraw") };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_launch_from_home_is_not_about_home() {
        let home = std::env::temp_dir();
        let arguments = [OsString::from("tuni")];
        assert_eq!(
            launch_directory(Some(home.clone()), &arguments, Some(home)),
            None
        );
    }

    #[test]
    fn an_argument_names_the_directory_relative_to_the_launch() {
        let cwd = std::env::temp_dir();
        let name = format!("tuni-launch-{}", std::process::id());
        std::fs::create_dir_all(cwd.join(&name)).expect("a directory to name");
        let arguments = [
            OsString::from("tuni"),
            OsString::from("--new-window"),
            OsString::from(&name),
        ];
        let found = launch_directory(Some(cwd.clone()), &arguments, None);
        let _ = std::fs::remove_dir(cwd.join(&name));
        assert_eq!(found, Some(cwd.join(name)));
    }
}
