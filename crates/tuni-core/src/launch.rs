//! What a launch decides before there is a window: whether it joins the Tuni
//! already running, which directory it was about, and the environment the
//! process and its first shell start with.
//!
//! Here rather than in the application because two programs ask. `tuni` is a
//! launcher linked against GLib alone, which starts the first window's shell
//! and then becomes `tuni-gtk`; `tuni-gtk` is the application, and run by
//! itself it has to arrive at the same answers.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

/// The application id: the bus name a running Tuni owns, the desktop entry and
/// the icon.
pub const APP_ID: &str = "dev.unisic.Tuni";

/// The id this process runs as: `TUNI_APP_ID` when set, else `APP_ID` for an
/// installed Tuni and `APP_ID.Devel` for one run out of its build directory.
/// A build being worked on is an application of its own, with a bus name, a
/// taskbar entry and a session of its own, so trying it never opens a project
/// in the Tuni somebody is working in.
///
/// Installed means the desktop entry is beside the binary, where every `make
/// install` puts it: `<prefix>/bin/tuni` and
/// `<prefix>/share/applications/dev.unisic.Tuni.desktop`. A build directory
/// has no such neighbour.
#[must_use]
pub fn app_id() -> String {
    if let Some(id) = std::env::var("TUNI_APP_ID")
        .ok()
        .filter(|id| !id.is_empty())
    {
        return id;
    }
    let installed = std::env::current_exe()
        .ok()
        .and_then(|exe| desktop_entry_beside(&exe))
        .is_some_and(|entry| entry.is_file());
    if installed {
        APP_ID.to_owned()
    } else {
        format!("{APP_ID}.Devel")
    }
}

/// Whether this is a build being worked on: one running as an id of its own.
#[must_use]
pub fn development() -> bool {
    static DEVELOPMENT: LazyLock<bool> = LazyLock::new(|| app_id() != APP_ID);
    *DEVELOPMENT
}

/// Where an installed binary's desktop entry is: the `share/applications` of
/// the prefix whose `bin` holds it.
fn desktop_entry_beside(exe: &Path) -> Option<PathBuf> {
    let prefix = exe.parent()?.parent()?;
    Some(
        prefix
            .join("share")
            .join("applications")
            .join(format!("{APP_ID}.desktop")),
    )
}

/// Whether this run must not join the one already going.
///
/// A capture renders a window and quits, and forwarded to a running instance it
/// would render nothing and quit anyway. A build being worked on is an
/// application of its own already (see `app_id`), but its second launch would
/// still be a project in its first, and `make dev` wants each launch to be the
/// new build, in a window of its own.
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
/// It also keeps GTK off `ext-background-effect-v1`, which GTK 4.23.3 and later
/// bind whenever the compositor offers it, so the window's blur stays with the
/// application's own request. GTK sends the region measured from the corner of
/// the surface, as the protocol says, and KWin 6.8 reads it from the corner of
/// the window instead, which on a window with a client-side shadow shifts the
/// blur right and down by the shadow's width and leaves the strip along the top
/// and left edges unblurred. A surface may hold only one effect object, so the
/// application cannot ask beside GTK; with GTK kept off the protocol it asks
/// alone, measured the way KWin reads it. GTK reads the list once, when it
/// connects, and has honored it since 4.20, before it spoke the protocol.
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
    let disabled = std::env::var_os(GDK_WAYLAND_DISABLE).unwrap_or_default();
    if let Some(disabled) = without_background_effect(disabled) {
        // SAFETY: as above.
        unsafe { std::env::set_var(GDK_WAYLAND_DISABLE, disabled) };
    }
}

/// The list of Wayland interfaces GTK leaves unbound.
const GDK_WAYLAND_DISABLE: &str = "GDK_WAYLAND_DISABLE";

/// `disabled` with the background effect manager added, or `None` when it is
/// already on the list. GTK splits the list on any of `:;, \t`.
fn without_background_effect(mut disabled: OsString) -> Option<OsString> {
    const MANAGER: &str = "ext_background_effect_manager_v1";
    let listed = disabled
        .to_string_lossy()
        .split([':', ';', ',', ' ', '\t'])
        .any(|name| name == MANAGER);
    if listed {
        return None;
    }
    if !disabled.is_empty() {
        disabled.push(",");
    }
    disabled.push(MANAGER);
    Some(disabled)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_installed_binary_has_its_desktop_entry_beside_it() {
        assert_eq!(
            desktop_entry_beside(Path::new("/usr/bin/tuni")),
            Some(PathBuf::from(
                "/usr/share/applications/dev.unisic.Tuni.desktop"
            ))
        );
        // A build directory has no `share` beside its `release`.
        assert_eq!(
            desktop_entry_beside(Path::new("/home/me/.cache/tuni-target/release/tuni")),
            Some(PathBuf::from(
                "/home/me/.cache/tuni-target/share/applications/dev.unisic.Tuni.desktop"
            ))
        );
    }

    #[test]
    fn gtk_is_kept_off_the_background_effect_once() {
        let manager = "ext_background_effect_manager_v1";
        assert_eq!(
            without_background_effect(OsString::new()),
            Some(OsString::from(manager))
        );
        assert_eq!(
            without_background_effect(OsString::from("wp_color_manager_v1")),
            Some(OsString::from(format!("wp_color_manager_v1,{manager}")))
        );
        // The second call, by the program the launcher becomes, adds nothing.
        assert_eq!(without_background_effect(OsString::from(manager)), None);
        assert_eq!(
            without_background_effect(OsString::from(format!("a:{manager} b"))),
            None
        );
    }

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
