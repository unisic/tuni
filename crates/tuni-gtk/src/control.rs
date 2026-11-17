//! The window half of `dev.unisic.Tuni.Remote`. What it offers, and why it is
//! off until somebody turns it on, is in [`tuni_core::control`].

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};
use tuni_core::control;
use tuni_core::usage::{Agent, Reader};
use tuni_core::workspace::Id;

use crate::terminal::TuniTerminal;
use crate::window::TuniWindow;

thread_local! {
    /// The object while the setting is on. One per process, not per window:
    /// the interface answers for every window at once.
    static REGISTERED: RefCell<Option<gio::RegistrationId>> = const { RefCell::new(None) };
}

/// Puts the object on the bus or takes it off, to match the setting. Every
/// window calls this when its settings change, so a call that changes nothing
/// is the usual one.
pub fn sync(app: &gtk::Application, on: bool) {
    let (Some(connection), Some(base)) = (app.dbus_connection(), app.dbus_object_path()) else {
        return;
    };
    REGISTERED.with_borrow_mut(|slot| match (on, slot.take()) {
        (true, Some(id)) => *slot = Some(id),
        (false, Some(id)) => {
            let _ = connection.unregister_object(id);
        }
        (true, None) => {
            let path = format!("{base}/{}", control::PATH);
            match register(app, &connection, &path) {
                Ok(id) => *slot = Some(id),
                Err(error) => eprintln!("cannot offer remote control: {error}"),
            }
        }
        (false, None) => {}
    });
}

fn register(
    app: &gtk::Application,
    connection: &gio::DBusConnection,
    path: &str,
) -> Result<gio::RegistrationId, glib::Error> {
    let node = gio::DBusNodeInfo::for_xml(control::XML)?;
    let interface = node
        .lookup_interface(control::INTERFACE)
        .ok_or_else(|| invalid("no interface"))?;
    let app = app.downgrade();
    // One reader for every `Usage` call, so a log is read from where the last
    // call left it and the plan is asked at most once a minute, as the Info
    // panel's own reader does.
    let reader = Rc::new(RefCell::new(Reader::default()));
    connection
        .register_object(path, &interface)
        .method_call(move |_, _, _, _, method, parameters, invocation| {
            let Some(app) = app.upgrade() else {
                invocation.return_result(Err(invalid("Tuni is closing")));
                return;
            };
            let windows: Vec<TuniWindow> = app
                .windows()
                .into_iter()
                .filter_map(|window| window.downcast().ok())
                .collect();
            if method == "Usage" {
                let started = args::<(u64,)>(&parameters)
                    .and_then(|(pane,)| usage_source(&windows, Id::from(pane)));
                match started {
                    Ok(source) => {
                        invocation.return_future_local(usage(source, reader.clone()));
                    }
                    Err(error) => invocation.return_result(Err(error)),
                }
                return;
            }
            invocation.return_result(call(&windows, method, &parameters));
        })
        .build()
}

fn call(
    windows: &[TuniWindow],
    method: &str,
    parameters: &glib::Variant,
) -> Result<Option<glib::Variant>, glib::Error> {
    match method {
        "State" => {
            let states: Vec<_> = windows.iter().map(TuniWindow::remote_state).collect();
            Ok(Some(control::state_json(&states).to_variant()))
        }
        "Text" => {
            let (pane, lines) = args::<(u64, u32)>(parameters)?;
            let text = terminal(windows, pane)?.text(control::text_lines(lines));
            Ok(Some(text.unwrap_or_default().to_variant()))
        }
        "Type" => {
            let (pane, text) = args::<(u64, String)>(parameters)?;
            if text.len() > control::TYPE_MAX {
                return Err(invalid("text over 64 KiB"));
            }
            terminal(windows, pane)?.paste_text(&text);
            Ok(None)
        }
        "Key" => {
            let (pane, key) = args::<(u64, String)>(parameters)?;
            let sent = control::key(&key).ok_or_else(|| invalid("unknown key"))?;
            terminal(windows, pane)?.send_text(sent);
            Ok(None)
        }
        "Focus" => {
            let (pane,) = args::<(u64,)>(parameters)?;
            let pane = Id::from(pane);
            find(windows, |window| window.holds_pane(pane))?.reveal_pane(pane);
            Ok(None)
        }
        "NewTab" => {
            let (project,) = args::<(u64,)>(parameters)?;
            let project = Id::from(project);
            find(windows, |window| window.holds_project(project))?.open_tab(project);
            Ok(None)
        }
        "CloseTab" => {
            let (tab,) = args::<(u64,)>(parameters)?;
            let tab = Id::from(tab);
            if !windows.iter().any(|window| window.close_tab(tab)) {
                return Err(invalid("no such tab"));
            }
            Ok(None)
        }
        _ => Err(glib::Error::new(gio::DBusError::UnknownMethod, method)),
    }
}

/// GDBus has already held the call to the signature in [`control::XML`], so
/// this failing is a description and a parser out of step.
fn args<T: glib::variant::FromVariant>(parameters: &glib::Variant) -> Result<T, glib::Error> {
    parameters
        .get::<T>()
        .ok_or_else(|| invalid("arguments do not match the method"))
}

fn find(
    windows: &[TuniWindow],
    holds: impl Fn(&TuniWindow) -> bool,
) -> Result<&TuniWindow, glib::Error> {
    windows
        .iter()
        .find(|window| holds(window))
        .ok_or_else(|| invalid("no such id in any window"))
}

fn terminal(windows: &[TuniWindow], pane: u64) -> Result<TuniTerminal, glib::Error> {
    windows
        .iter()
        .find_map(|window| window.terminal(Id::from(pane)))
        .ok_or_else(|| invalid("no terminal pane with that id"))
}

/// What a usage read needs from the main thread: the shell to look under,
/// the directory some agents' logs are filed by, and the settings that say
/// which agents may be read at all.
struct UsageSource {
    shell: u32,
    cwd: PathBuf,
    agents: tuni_core::settings::Agents,
}

fn usage_source(windows: &[TuniWindow], pane: Id) -> Result<UsageSource, glib::Error> {
    let (window, terminal) = windows
        .iter()
        .find_map(|window| window.terminal(pane).map(|terminal| (window, terminal)))
        .ok_or_else(|| invalid("no terminal pane with that id"))?;
    Ok(UsageSource {
        shell: terminal.shell_pid().unwrap_or(0),
        cwd: terminal.cwd().map(PathBuf::from).unwrap_or_default(),
        agents: window.settings().agents,
    })
}

/// The Info panel's read, for a pane that need not be on screen: the process
/// walk and the log reading go to a worker thread, as there.
async fn usage(
    source: UsageSource,
    reader: Rc<RefCell<Reader>>,
) -> Result<Option<glib::Variant>, glib::Error> {
    let taken = reader.take();
    let read = gio::spawn_blocking(move || {
        let mut reader = taken;
        let snapshot = if source.shell == 0 {
            None
        } else {
            Some(tuni_core::info::snapshot(source.shell))
        };
        let agents = source.agents;
        let usage = snapshot
            .and_then(|snapshot| Agent::running(&snapshot.processes))
            .filter(|_| agents.usage)
            .filter(|(agent, _)| agent.watched(&agents))
            .map(|(agent, pid)| {
                reader.read(agent, pid, &source.cwd, agents.plan, agents.usagefleet)
            })
            .unwrap_or_default();
        (usage, reader)
    })
    .await
    .map_err(|_| invalid("the usage read failed"))?;
    let (usage, taken) = read;
    reader.replace(taken);
    Ok(Some(control::usage_json(&usage).to_variant()))
}

fn invalid(message: &str) -> glib::Error {
    glib::Error::new(gio::DBusError::InvalidArgs, message)
}
