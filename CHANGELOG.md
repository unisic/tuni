# Changelog

What changed for someone using Tuni, newest first. The release workflow
publishes a version's section as the notes of its GitHub release, so the
section has to exist before the tag does. Releases before 1.7.0 are described
in `data/dev.unisic.Tuni.metainfo.xml`.

## 1.7.1

### Added

- Ctrl+Shift+J goes to the next tab that needs you, one a bell, a long command
  that ended or a finished agent marked, and lands on the pane the agent
  finished in. The foot of the sidebar counts those tabs on a button that does
  the same.
- A clipboard holding a screenshot and no text pastes as the quoted path of a
  file holding the image, so it can be handed to a coding agent without saving
  it first. Pastes older than a week are removed.
- The palette runs the checkout's own tasks: `package.json` scripts with the
  package manager the project names, `justfile` recipes, and `Makefile`,
  `Taskfile` and `mise.toml` tasks. Each opens a shell beside the pane on
  screen, in the directory that declares it.
- The palette opens a dev server's port in the browser, opens the checkout in
  Files or an installed editor, and resumes one of the checkout's five latest
  Claude Code conversations.
- New Tab in Worktree opens a tab on a branch checked out in a git worktree of
  its own under `.tuni/worktrees`, so agents side by side never edit the same
  files. Closing its last tab offers to remove the worktree, and its branch
  once every commit on it is merged, never by force.
- Split panes without the keyboard are dimmed, as in Ghostty, and
  focus-follows-mouse hands the keyboard to the pane under the pointer. Both
  are under Window in Preferences, and the second is off by default.
- A folder dropped on the sidebar opens as a project, the empty foot of the
  sidebar moves the window, and Close All Tabs in a project's menu closes
  every tab and keeps the project.
- Settings and commands are found by the words people use for them: the
  Preferences search looks through its rows, the palette opens a settings
  page by what is on it, and a command answers to its synonyms.
- The Info panel recognises rokartur's agent, with its sessions, usage and
  plan bars, and names who serves an agent's model, as in "Pi ·
  gemini-3.8-flash via antigravity".

### Improved

- The terminal core is Ghostty 1.4.0, built from its signed release rather
  than a development snapshot from before it. Searching a full scrollback of
  50,000 lines takes 25 ms instead of 45, and saving it with the session when
  the window closes 28 ms a pane instead of 70.
- A frame reads and lays out again only the rows that changed, so a screen
  that stays put while a spinner moves costs about half the CPU: 181 to 199 ms
  over ten seconds of a 30 Hz spinner instead of 313 to 476.
- A closed pane and a flood of output hand their memory back to the system
  once two seconds pass, instead of keeping it while the window is open.
- An agent that reports itself busy with OSC 9;4, as Pi does, drives the tab's
  spinner, the finished mark and the banner without being listed anywhere.
- A Claude Code pane's Info section reads the conversation running in that
  pane rather than the checkout's latest, and Pi's follows a model switch
  before the next answer and shows the plan bars on a Claude login.

### Fixed

- Output that cleared the screen into the scrollback could leave the cursor
  holding the style and hyperlink of a page already freed, so the next
  character printed wrote through memory that was no longer its own
  (GHSA-w9q4-5jvv-mcv4). Ghostty 1.4.0 fixes it.
- The Fedora package builds in COPR again. 1.7.0 stopped there because the
  build cloned Ghostty with git, which COPR's buildroot does not have.
- Claude Code 2.1.311's new title spinner counts as a turn, so its tab shows
  the spinner and the finished mark again.
- The plan bars are asked for as `tuni/<version>`, which the endpoint answers,
  instead of under curl's own name, which it refused with a 429.

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
