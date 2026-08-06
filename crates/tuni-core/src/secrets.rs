//! The password a host asks for, kept where the desktop keeps passwords.
//!
//! Tuni holds no secret, and this does not change that. What a saved password
//! costs is one dialog: the characters go from the entry into `secret-tool
//! store` and nothing here writes them to a file, keeps them in a field or
//! reads them back. `has` asks whether the keyring has one and is told yes or
//! no, never what it is. The only process that ever sees the password again is
//! `ssh` itself, through the askpass helper below, which tuni is not the parent
//! of the way it is not the parent of a `ControlMaster`.
//!
//! `secret-tool` is libsecret's own command, so this speaks the Secret Service
//! the desktop already runs — gnome-keyring, KWallet's `ksecretd`, whatever the
//! session provides — with no library linked in and no second store to unlock.
//! A machine without libsecret has no keyring, which is the same thing as
//! having no password saved: [`available`] says so and the editor stops
//! offering it.
//!
//! The helper is a shell script written into `$XDG_RUNTIME_DIR`, which is
//! tmpfs, 0700 and emptied at logout. It contains a lookup and no password, and
//! it answers a password prompt and nothing else: a prompt about a host key is
//! a question for the person at the keyboard, and answering that one from a
//! script is how a client accepts a machine-in-the-middle without telling
//! anybody. [`askpass`] refuses to arm it at all until `ssh` already knows the
//! host's key.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// What the entries are filed under, so a password tuni saved is one a person
/// can find, audit and delete in Seahorse or KWalletManager.
const SERVICE: &str = "tuni-ssh";

/// Whether this desktop has anywhere to put a password.
#[must_use]
pub fn available() -> bool {
    tool().is_some()
}

/// Whether the keyring holds a password for `destination`.
///
/// The answer is an exit status. The password itself is read by `ssh`'s askpass
/// and never by this process.
#[must_use]
pub fn has(destination: &str) -> bool {
    let Some(tool) = tool() else {
        return false;
    };
    Command::new(tool)
        .args(["lookup", "service", SERVICE, "host", destination])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Puts `password` in the keyring under `destination`, replacing whatever was
/// there.
///
/// On stdin rather than in an argument: an argument is in `/proc/<pid>/cmdline`
/// for anything on the machine to read.
pub fn store(destination: &str, password: &str) -> Result<(), String> {
    let tool = tool().ok_or_else(missing)?;
    let mut child = Command::new(tool)
        .args([
            "store",
            "--label",
            &format!("SSH password for {destination}"),
            "service",
            SERVICE,
            "host",
            destination,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("secret-tool: {error}"))?;
    child
        .stdin
        .take()
        .ok_or_else(|| "secret-tool: no stdin".to_owned())?
        .write_all(password.as_bytes())
        .map_err(|error| format!("secret-tool: {error}"))?;
    let output = child
        .wait_with_output()
        .map_err(|error| format!("secret-tool: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(reason(&output.stderr))
    }
}

/// Takes the password for `destination` out of the keyring. Succeeds when there
/// was none, because what the caller asked for is that there be none.
pub fn clear(destination: &str) -> Result<(), String> {
    let tool = tool().ok_or_else(missing)?;
    let output = Command::new(tool)
        .args(["clear", "service", SERVICE, "host", destination])
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("secret-tool: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(reason(&output.stderr))
    }
}

/// The environment that lets `ssh` answer its own password prompt for
/// `destination`, or nothing at all, which is a pane that asks in the terminal
/// the way it always has.
///
/// Nothing at all is the answer unless every one of these is true: a password
/// is saved, the helper could be written, and `ssh` already knows the host key.
/// The last one is the important one. `SSH_ASKPASS_REQUIRE=force` sends every
/// prompt to the helper, including the one asking whether an unknown machine is
/// the right machine, and that question belongs to the person connecting.
#[must_use]
pub fn askpass(destination: &str) -> Vec<(String, String)> {
    if !has(destination) || !known_host(destination) {
        return Vec::new();
    }
    let Some(helper) = helper() else {
        return Vec::new();
    };
    vec![
        (
            "SSH_ASKPASS".to_owned(),
            helper.to_string_lossy().into_owned(),
        ),
        // `force` rather than `prefer`, because `prefer` is documented against
        // `DISPLAY` and a Wayland session has none to offer.
        ("SSH_ASKPASS_REQUIRE".to_owned(), "force".to_owned()),
        ("TUNI_SSH_HOST".to_owned(), destination.to_owned()),
    ]
}

/// Whether `ssh` would connect to `destination` without asking anybody about
/// its key. `ssh-keygen -F` is the same `known_hosts` lookup `ssh` does,
/// hashed entries included.
fn known_host(destination: &str) -> bool {
    let host = destination
        .rsplit_once('@')
        .map_or(destination, |(_, host)| host);
    Command::new("ssh-keygen")
        .args(["-F", host])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// Writes the askpass helper and returns where it is, or nothing if it could
/// not be written.
///
/// Rewritten every time rather than reused: it is three lines, and a script in
/// a directory that survives an upgrade is a script an older tuni left behind.
fn helper() -> Option<PathBuf> {
    let directory = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| crate::settings::home().join(".cache"))
        .join("tuni");
    std::fs::create_dir_all(&directory).ok()?;
    let path = directory.join("askpass");
    // The prompt `ssh` is asking arrives as the first argument, and only a
    // password prompt is answered: a passphrase belongs to `ssh-agent` and a
    // host key question belongs to the person at the keyboard. Refusing is an
    // exit status, which `ssh` reads as an unanswered prompt.
    let script = "#!/bin/sh\n\
         # Written by tuni. Holds no secret: it asks the desktop keyring for\n\
         # the password of the host tuni was asked to connect to.\n\
         [ -n \"$TUNI_SSH_HOST\" ] || exit 1\n\
         case \"$1\" in\n\
         *assword*) ;;\n\
         *) exit 1 ;;\n\
         esac\n\
         exec secret-tool lookup service tuni-ssh host \"$TUNI_SSH_HOST\"\n";
    // Written beside itself and renamed on, so an `ssh` reading it while this
    // rewrites it cannot run half a script.
    let temporary = path.with_extension("new");
    std::fs::write(&temporary, script).ok()?;
    // Nobody but the owner runs it, and nobody at all writes it: an askpass a
    // second user could edit is a password handed to that user.
    std::fs::set_permissions(
        &temporary,
        <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
    )
    .ok()?;
    std::fs::rename(&temporary, &path).ok()?;
    Some(path)
}

/// `secret-tool`, if this machine has it. Looked up per call rather than
/// remembered, the same as the editor table: a handful of `stat` calls is
/// cheaper than being wrong after somebody installs libsecret.
fn tool() -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|directory| directory.join("secret-tool"))
        .find(|path| {
            std::fs::metadata(path).is_ok_and(|meta| {
                meta.is_file()
                    && <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::mode(
                        &meta.permissions(),
                    ) & 0o111
                        != 0
            })
        })
}

fn missing() -> String {
    "secret-tool is not installed, so this desktop has no keyring to ask".to_owned()
}

/// The first line of what the tool complained about, which is the part that
/// names the problem; the rest is usage.
fn reason(stderr: &[u8]) -> String {
    String::from_utf8_lossy(stderr)
        .lines()
        .next()
        .unwrap_or("secret-tool failed")
        .to_owned()
}

#[cfg(test)]
mod tests {
    /// The helper is the one place a mistake is silent: a script that answers
    /// every prompt would hand the password to a host key question, and a
    /// question nobody sees answered is a machine-in-the-middle nobody sees.
    #[test]
    fn the_helper_answers_a_password_prompt_and_refuses_the_rest() {
        let Some(helper) = super::helper() else {
            return;
        };
        let ask = |prompt: &str| {
            std::process::Command::new(&helper)
                .arg(prompt)
                .env("TUNI_SSH_HOST", "")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .expect("run the helper")
                .success()
        };
        // With no host named, every prompt is refused, which is what the empty
        // environment above is asking about: the refusal, not the lookup.
        assert!(!ask("dean@10.0.0.1's password: "));
        assert!(!ask(
            "The authenticity of host '10.0.0.1' can't be established."
        ));
        assert!(!ask(
            "Enter passphrase for key '/home/dean/.ssh/id_ed25519': "
        ));
    }

    /// The round trip against the keyring the desktop is actually running.
    ///
    /// Ignored by default: it wants a session bus and a Secret Service, and on
    /// a machine that has one it may want it unlocked, which is a dialog no
    /// test suite should be able to raise. Run it by hand with
    /// `cargo test -p tuni-core -- --ignored`.
    #[test]
    #[ignore = "needs the desktop keyring"]
    fn a_password_goes_into_the_keyring_and_comes_back_out_of_it() {
        let host = "tuni-test.invalid";
        assert!(super::available(), "secret-tool is not installed");
        super::store(host, "hunter2").expect("store the password");
        assert!(super::has(host));
        super::clear(host).expect("clear the password");
        assert!(!super::has(host));
    }
}
