# Changelog

What changed for someone using Tuni, newest first. The release workflow
publishes a version's section as the notes of its GitHub release, so the
section has to exist before the tag does. Releases before 1.7.0 are described
in `data/dev.unisic.Tuni.metainfo.xml`.

## 1.7.0

### Added

- A host's files open in a pane beside this machine's: the Files tree on one
  side, the remote folder on the other, and a drag copies either way.
- Whole folders copy and delete over SFTP, and a remote file opens in the
  editor and saves back.
- Remote files show their size, date and permissions, and several can be acted
  on at once.
- The transfers waiting are listed, each with a way to cancel it, and a
  transfer asks before it replaces a file of the same name.
- Hosts sit in groups, and the list filters by a tag.
- A host can paint its panes in a theme of its own.
- The host editor switches agent forwarding and environment variables on, and
  the SSH settings are gathered under Session in Preferences.
- A snippet can leave blanks that are asked for before it runs, Ctrl+Shift+S
  opens the snippets alone, and one snippet can run on every host the list
  shows.
- A dropped connection dials again by itself when the network is back.
- A changed host key can be forgotten from the bar that reports it.
- A key can be made on a FIDO2 security key.
- The "+" in the tab strip offers the five hosts connected to last.
- Fedora can install Tuni from COPR and update it with the rest of the system:
  `sudo dnf copr enable debondor/Tuni && sudo dnf install tuni`.

### Improved

- The shell starts about 5 ms after launch instead of 660, before GTK is
  loaded, and the first frame is up in 234 ms instead of 671.
- A flood of output is read in bursts: 146.4 MiB/s instead of 112.0, first of
  nineteen terminals measured in one run.
- The window holds a fifth less memory: 47.1 MiB PSS instead of 59.8.
- Idle panes wake the window together: sixteen of them cost 22 wakeups in 20
  seconds instead of 92.
- SFTP keeps sixty-four requests in flight instead of one, so an 8 MiB file
  over a link with 50 ms added comes down in 0.9 s instead of 16.6.
- Browsing a host goes on while a file moves to or from it, instead of every
  click waiting for the whole file.
- A remote file is replaced in one step where the server offers it, so nothing
  on the far end finds the name missing in between.
- With the Discord presence on and Discord closed, Tuni no longer runs git
  every fifteen seconds to build an activity nobody receives.

### Fixed

- Cancelling an upload over an existing file no longer deletes that file.
- A download no longer replaces a symlink with a plain file, and a file
  fetched over itself keeps its mode, so a script stays executable.
- A connection cut partway through a listing no longer shows half a folder as
  all of it: the page says why and offers Reconnect.
- The file browser says why it could not connect, instead of blaming the
  password every time.
- Saving a host keeps every `IdentityFile` it had and a jump chain such as
  `gate,inner` or `user@gate:2222`.
- A `Host` or `Match` line among a host's extra options is refused however it
  is spelled, `Host=*` and `"Host" *` included.
- A shared connection set to linger zero seconds closes with its last pane
  instead of staying forever.
- A host's on-connect snippet is typed only into its own pane, even with
  broadcast input on, and only once that pane has logged in.
- A saved password is offered to a host whose key ssh files under its
  HostName, a port other than 22 or a `HostKeyAlias`.
- Lines after an `Include` in `~/.ssh/config` stay with the host they were
  written under.
- An address typed with a port, such as `deploy@10.0.0.1:2222`, no longer
  reads as disconnected while its connection is up.
