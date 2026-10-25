//! The program a launch runs: it starts the first window's shell, then becomes
//! `tuni-gtk` beside it.
//!
//! A separate program because the dynamic linker is the first cost of a GTK
//! application, before any of its code runs: from `exec` to `main` is about
//! 18 ms with libadwaita's 134 libraries bound, against 1 ms for this one,
//! which links GLib and GIO alone. A shell started here loads its startup files
//! while all of that happens, and `tuni-gtk` adopts it. It is a guess at what
//! the first window will ask for, and `TuniTerminal::start` takes it only when
//! the shell it would have started is this one. A wrong guess is a shell that
//! read its startup files and was hung up, so there is no guess where the
//! answer is not cheap to know here: a session about to be restored, whose
//! panes are wherever they were left, or a launch about to hand itself to the
//! Tuni already running, which opens no window of its own. Two launches racing
//! for the bus can both get past that, and the one that loses hangs up a shell
//! nobody saw.

use std::ffi::OsString;
use std::os::unix::process::CommandExt as _;
use std::path::PathBuf;

use gio::prelude::*;
use tuni_core::launch::{self, APP_ID};
use tuni_core::settings::Settings;
use tuni_pty::{Pty, PtyConfig};

fn main() -> std::process::ExitCode {
    let arguments: Vec<OsString> = std::env::args_os().collect();
    let app = match std::env::current_exe() {
        Ok(path) => path.with_file_name("tuni-gtk"),
        Err(e) => {
            eprintln!("tuni: cannot find tuni-gtk: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let mut next = std::process::Command::new(&app);
    next.args(arguments.iter().skip(1));
    // The name it was run under, so `tuni-gtk` reads the command line this
    // launch had: GLib takes the program's name from it.
    if let Some(name) = arguments.first() {
        next.arg0(name);
    }

    let settings = Settings::load();
    // SAFETY: the first thing this program does that is not a read. Asking the
    // bus below starts GDBus's thread, so it has to come first.
    unsafe { launch::quieten(settings.terminal.background_opacity) };
    if (launch::separate_process() || !primary_running())
        && let Some(config) = first_shell(&settings, &arguments)
        && let Ok((master, pid)) = Pty::start(&config)
    {
        // A shell that cannot cross the `exec` is hung up as this process
        // turns into the next one and closes the master.
        let _ = Pty::hand_over(&config, master, pid, &mut next);
    }

    let e = next.exec();
    eprintln!("tuni: cannot run {}: {e}", app.display());
    std::process::ExitCode::FAILURE
}

/// The shell the first window will ask for, unless that window is going to be
/// a restored session.
fn first_shell(settings: &Settings, arguments: &[OsString]) -> Option<PtyConfig> {
    if tuni_core::session::enabled()
        && settings.restore_session
        && tuni_core::session::Snapshot::path().exists()
    {
        return None;
    }
    // Where `build_window` opens its project without a session, and so where
    // `directory_for_new_shell` starts the first shell: the directory the
    // launch named, else the process's own. Each is read the way that path
    // reads it, `g_get_current_dir` for the one and `getcwd` for the other,
    // since they differ under a symlink.
    let cwd = launch::launch_directory(
        Some(gio::glib::current_dir()),
        arguments,
        std::env::var_os("HOME").map(PathBuf::from),
    )
    .or_else(|| std::env::current_dir().ok());
    Some(PtyConfig {
        shell: tuni_pty::resolve_shell(&settings.terminal.command),
        cwd,
        ..PtyConfig::default()
    })
}

/// Whether the application id is already on the session bus, which is the
/// question `GApplication` in `tuni-gtk` is about to ask it before handing
/// this launch over. No answer counts as yes, since the cost of a wrong yes is
/// only a shell started at the usual time.
fn primary_running() -> bool {
    let Ok(bus) = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) else {
        return true;
    };
    bus.call_sync(
        Some("org.freedesktop.DBus"),
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
        "NameHasOwner",
        Some(&(APP_ID,).to_variant()),
        None,
        gio::DBusCallFlags::NONE,
        -1,
        gio::Cancellable::NONE,
    )
    .ok()
    .and_then(|reply| reply.get::<(bool,)>())
    .is_none_or(|(owned,)| owned)
}
