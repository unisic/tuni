//! Discord Rich Presence, kept off the main thread: the socket can stall for
//! seconds, and what the presence says costs a git run and a `/proc` read.

use std::cell::OnceCell;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tuni_core::discord::{
    Activity, CLIENT_ID, DiscordIpc, PresenceStatus, format_activity, look_up,
};
use tuni_core::settings::Discord;

/// How long a change waits before it goes out, so a burst of them (a prompt
/// redraws the title and the directory together) is sent once. Discord takes
/// five activity updates in twenty seconds and drops the rest, so this is also
/// the most often one is sent.
const SETTLE: Duration = Duration::from_secs(4);

/// How long until a Discord that was not there is looked for again: one
/// `connect` per socket file that exists, which is none while it is closed.
const RETRY: Duration = Duration::from_secs(15);

type Wanted = Option<(PresenceStatus, Discord)>;

thread_local! {
    /// One worker per process rather than one per window: Discord keeps one
    /// activity per pid, and two windows each holding a connection would take
    /// turns overwriting it.
    static WORKER: OnceCell<Sender<Wanted>> = const { OnceCell::new() };
}

/// Says what the presence should be, or with `None` that there should be none.
/// A `None` while nothing was ever shown starts no thread.
pub fn show(wanted: Wanted) {
    WORKER.with(|worker| {
        if wanted.is_none() && worker.get().is_none() {
            return;
        }
        let sender = worker.get_or_init(|| {
            let (sender, receiver) = mpsc::channel();
            // A worker that did not start leaves a channel nobody reads, and a
            // presence missing is all that costs.
            let _ = thread::Builder::new()
                .name("tuni-discord".to_owned())
                .spawn(move || run(&receiver));
            sender
        });
        let _ = sender.send(wanted);
    });
}

/// The worker. The queue in front of it is unbounded, but it is drained down
/// to the newest message every pass, and a pass blocks for at most two socket
/// timeouts.
fn run(receiver: &Receiver<Wanted>) {
    let trace = std::env::var_os("TUNI_DEBUG_DISCORD").is_some();
    // The elapsed clock counts from when the presence first went up, not from
    // the last pane switch, which would keep it near zero.
    let start = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let mut wanted: Wanted = None;
    let mut due: Option<Instant> = None;
    let mut connection: Option<DiscordIpc> = None;
    let mut sent: Option<Activity> = None;

    loop {
        let message = match due {
            Some(at) => receiver.recv_timeout(at.saturating_duration_since(Instant::now())),
            None => receiver.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        match message {
            Ok(next) => {
                wanted = next;
                due.get_or_insert_with(|| Instant::now() + SETTLE);
                continue;
            }
            Err(RecvTimeoutError::Timeout) => due = None,
            Err(RecvTimeoutError::Disconnected) => return,
        }

        let Some((status, show)) = &wanted else {
            if let Some(mut ipc) = connection.take() {
                let _ = ipc.set_activity(None);
                let _ = ipc.close();
                if trace {
                    eprintln!("[discord] presence cleared");
                }
            }
            sent = None;
            continue;
        };
        // Connected before looked up, since the look-up is three `git` runs
        // and a Discord that is closed would otherwise cost them every retry,
        // for as long as the window is open, for a presence that goes nowhere.
        let ipc = match &mut connection {
            Some(ipc) => ipc,
            None => match DiscordIpc::connect(CLIENT_ID) {
                Ok(ipc) => connection.insert(ipc),
                Err(error) => {
                    if trace {
                        eprintln!("[discord] not connected: {error}");
                    }
                    due = Some(Instant::now() + RETRY);
                    continue;
                }
            },
        };
        let activity = format_activity(status, &look_up(status, *show), *show, start);
        if sent.as_ref() == Some(&activity) {
            continue;
        }
        match ipc.set_activity(Some(&activity)) {
            Ok(_) => {
                if trace {
                    eprintln!(
                        "[discord] {:?} / {:?}",
                        activity.details.as_deref().unwrap_or(""),
                        activity.state.as_deref().unwrap_or("")
                    );
                }
                sent = Some(activity);
            }
            // Sending the same activity again would be refused again.
            Err(error) if error.kind() == std::io::ErrorKind::InvalidInput => {
                if trace {
                    eprintln!("[discord] activity refused: {error}");
                }
                sent = Some(activity);
            }
            Err(error) => {
                if trace {
                    eprintln!("[discord] connection lost: {error}");
                }
                connection = None;
                sent = None;
                due = Some(Instant::now() + RETRY);
            }
        }
    }
}
