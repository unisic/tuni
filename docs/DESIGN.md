# Tuni in detail

Everything the window does, why it is built the way it is, and every key it
answers to. [README.md](../README.md) is the short version.

A native terminal workspace for Linux: terminal panes, projects, a file tree, a
git panel, a session inspector, an editor, and a diff viewer in one window.

Tuni is a ground-up Linux implementation of the workspace that
[egoist/kero](https://github.com/egoist/kero) built for macOS. Kero's Swift
source is read as a specification of behavior, not translated; the macOS build
is SwiftUI/AppKit over the libghostty embed API, which exists only for macOS and
iOS. Kero is GPLv3 and so is Tuni.

## Status

A window of projects and tabs. The terminal itself: keyboard input, Pango
rendering, mouse selection with word and line clicks, clipboard and bracketed
paste, SGR mouse reporting and focus reporting for applications that ask for
them, an overlay
scrollbar that fades when idle, a cursor that blinks by the desktop's own
preference, OSC 8 hyperlinks and plain-text URLs opened with `Ctrl`+click,
inline images over the
kitty graphics protocol, a configurable font with live zoom, and Ghostty's 574
color themes, which paint the window chrome as well as the terminal. `ls`,
`vi`, and `top` all render correctly.

Around it: a sidebar of projects, each with its own strip of tabs, and inside a
tab a niri-style layout of panes. A project is named by whatever its visible
shell calls itself until you rename it, which opens a prompt with the keyboard
already in it and the old name selected, so the new one is typed and entered
without touching the mouse again. A project directory can be pinned for the
file tree and the git panel to stay on. A new tab starts where the visible one
is, opens next to it, and closes when its last shell exits. Where it starts is
a setting: `new-tab-inherit-directory` off opens every new tab and split where
a shell started outside the window would, and "Do Not Follow the Shell's
Directory" in a project's own menu takes one project out of it while the rest
go on following, since the exception is usually one project rather than the
habit. A project with nothing to follow yet has two answers rather than one:
the project the window opens with starts in the directory tuni itself was
started in, since running it from a checkout should open there, and every
project opened after that starts at home, because a second project is a fresh
start and not a continuation of whatever launched the window. A project's row
draws a folder until its menu is asked for another one, which opens forty emoji
first and the icons from the desktop's theme under them, since half of what
makes one row findable is a color the other rows do not have and the colored
half should not be the half behind a button. Every other emoji this desktop has
is the last tile of that grid, which opens the system chooser with its search
field. One field holds both and which of the two it is is read off the string
rather than stored beside it: an icon name is ASCII, an emoji is not, so there
is no pair of settings that can disagree about what a row shows. A themed icon
is drawn in the row's own foreground color like every other symbolic icon in the
window; an emoji is text the font colors. A project whose tabs are all closed
stays in the sidebar until it is closed on purpose: by its own button, by its
menu, or by a middle click anywhere on the row, which is how a tab closes
everywhere else. Dragged off the sidebar entirely it becomes a window of its own
instead, tabs, panes, shells and all: the tab strip does this for one tab and
hears it from `AdwTabView`, and a list row, which has no such signal, reads a
drag that ended on nothing as the same request. The sidebar and
the panel are both dragged to a width by their inner edge, and one dragged to a
width keeps it. Until then each is a fraction of the window, which is what a
split view sizes by and what an undragged one should go on doing.

On the other side, a panel under `Ctrl+Shift+B` shows the directory the focused
shell is working in, or the project's own if one is pinned, and follows it as
the focus moves. It stands beside the tab strip rather than under it, since the
strip names the terminals and the panel is the same set of pages whichever tab
is in front. Its Files page opens a directory in place under the chevron beside
it and a file in a pane of its own, a double click on a directory makes it the
root instead, and a right click opens a file beside what is already there,
renames, creates, copies a path, shows a file in the desktop's file manager,
moves one to the trash, or types a `cd` into the terminal that has the keyboard.
The header steps to the parent directory, or takes a typed path the way a prompt
would, `~` included, and the two buttons under a thumb walk back and forward
through the directories that have been the root, which is what a file manager
and a browser both spend them on. Wandering off holds until the window has an
actually different directory to show, so the focus moving between panes of one
project does not snap the tree back.

Its Git page, under `Ctrl+Shift+G`, is the repository that directory belongs to:
the branch and how far it is from its upstream, what is in conflict, what is
staged, what has changed, and the last few commits. Files stage, unstage and
discard one at a time or all at once, a message commits what is staged or
everything, and fetch, pull, push and stash are a menu on the header, where
kero keeps them: five spelled-out buttons are wider than the panel is. Every one of them
is the `git` you would have typed, run as a process, so the repository ends up
in the state the command line would have left it in — including the reflog entry
to undo it by. Discarding asks first, and what git would delete outright goes to
the desktop's trash instead. The state of a file is spelled out, never carried
by color alone: the two porcelain letters are on the row, and what they mean is
in its tooltip.

Its Info page, under `Ctrl+Shift+I`, is the session itself: the shell and its
process id, the directory it is working in, the directory the other two pages
anchor to and whether that was pinned or worked out, every process running under
that shell with its share of a core and its resident memory, and every TCP port
those processes are listening on. A port is a button that opens
`http://localhost:<port>` in the browser, which is the reason to look — a dev
server prints its port and then scrolls away. A process can be asked to quit, or
made to. Nothing here shells out: it is `/proc`, read on a worker thread while
the page is showing and no more often than that.

When a coding agent is running under that shell, the page grows an Agent section
for it. Claude Code, Codex and OpenCode each keep a record of their own turns on
disk, and that is where the figures come from: what the session working in this
directory has spent, split into fresh input, output and what came back from the
cache, which is most of a long conversation and costs a fraction of the rest.
Above the tokens sit the plan's own windows, a bar per window with how much of
it is gone and when it starts over, which is the reason to look: the
alternative is a browser tab. Codex writes those percentages into its log along with how
full the model's context is; Claude Code's log does not carry them, so they are
asked of the account's usage endpoint, the numbers its own usage page shows,
with the login the agent already keeps on disk, at most once a minute, only
while the agent runs. That request is the one exception to everything staying
on this machine, it goes to the same place the agent itself talks to, and it
carries nothing that was not already in `~/.claude`. Tokens and percentages,
never prices, and nothing here signs in anywhere.

A pane holds a file as readily as it holds a shell. Opening one from either page
puts it where a terminal would have gone — syntax highlighting for whatever
GtkSourceView recognizes, line numbers, undo, find and replace, and `Ctrl+S`.
A file with unsaved edits carries a dot in its header and on its tab, and
nothing that would throw those edits away — closing the pane, the tab, or the
window — happens without asking first. A picture opens in the pane too, and
anything that is neither, or is past the 5 MiB the editor will read, says so and
offers to hand the file to the desktop.

When the machine has a language server for what the file is, the editor drives
it: a mistake is underlined where it sits, with a sign in the gutter and the
explanation in a popover when the pointer rests on it; completion opens on `.`,
`::` and `->` or on Ctrl+Space, and narrows as the word grows without asking
the server again; F12 or a Ctrl+click goes to where the thing under the cursor
is defined, opening the file it lands in. The server gets the same deal git and
ssh get, an external program found on `PATH` and driven as a process, so
completion in a Rust file is whatever `rust-analyzer` says, with the exact
configuration the command line's own tooling would have used. The built-in
table covers the servers that are useful over stdio with no configuration:
`rust-analyzer`, `clangd`, `gopls`, `zls`, and the Python, TypeScript, Lua and
shell ones. One server runs per project and language, shared by every pane
showing such a file, and hears about edits a beat behind the typing. Which
directory is the project is read from the language's own marker, `Cargo.toml`
or `go.mod` or `compile_commands.json`, then from `.git` when none is found,
because the server builds its whole world from that answer once. A machine
without a server simply has an editor without one, and closing the last file a
server was watching shuts the server down rather than keeping it warm.

A selection can follow the shape of the code rather than the reach of a drag.
Alt+Up selects the smallest piece of syntax around the cursor, and pressed
again the next one around that: the number, the expression, the statement, the
function. Alt+Down retraces the growth one step at a time, and an edit or a
click anywhere else retires the trail, since it described text that is gone.
The shape comes from a real parse, tree-sitter with the grammars compiled in
the way the themes are, covering most of the same languages the servers do; a
file outside that table keeps its ordinary selection keys and loses nothing
else.

Debugging is a page of the panel and a key in the editor. F8 puts a breakpoint
on the cursor's line, drawn as a dot in the gutter and remembered per file, so
a pane closed and reopened keeps its dots. The Debug page names a program, and
Start hands it to the adapter that debugs such things, lldb-dap for a native
binary and debugpy for a Python file, found on `PATH` and driven as a process
like every other tool here. A stop lands the editor on the stopped line; the
page shows the stack, a click opening any frame that has a file, the locals of
the top frame, and everything the program printed. Four buttons continue and
step, disabled while the program runs, because a step means nothing until it
stops. Delve is missing on purpose: `dlv dap` only listens on a socket, and
this client speaks stdio to a process it started.

A row on the Git page opens what changed in that file rather than the file, in a
pane of the same kind: the working tree against the index, or — for a row that is
already staged — the index against HEAD. Each hunk is headed by its `@@` line and
how much it adds and removes, both sides' line numbers run down the margin, and
the words that actually differ within a changed line are picked out of it. One
button swaps the inline reading for the two sides beside each other. The plus on
a hunk stages that hunk alone, and on the staged side takes it back out again;
either way it is a patch handed to `git apply`, so the index ends up where the
command line would have left it. What the shell beside the pane does to the file
lands in the diff on its own.

A pane holds a machine that is not this one just as readily. `Ctrl+Shift+O`
opens the host list: everything `~/.ssh/config` declares and everything added
in tuni, in one list, narrowed by the same typing the palette answers to, with
Enter connecting in this pane, `Ctrl+Enter` in a split beside it, and
`Shift+Enter` in a tab of its own. A dot says which machines are answering.
Setting `new-tab` to `hosts` makes that list what `Ctrl+Shift+T` opens, for
people who connect more often than they start a shell here.

None of the connecting is tuni's own work. Opening a host runs `ssh` in the
pane, so every prompt an authentication can raise, the unknown-host question
included, is asked in a terminal and answered there, and tuni sees no password
and no passphrase at any point. Panes on one host go through a single
authenticated connection, so a second tab costs no second login, and the Info
page says which host a pane is on, what its name resolves to, and how long that
shared connection has been up. Hosts added here go in a file of tuni's own; a
host declared in `~/.ssh/config` is read, listed and connected to but never
rewritten, and editing one opens that file at the line that declares it, in an
editor pane. A session that ends leaves its last screen where it is with a
Reconnect button over it, rather than closing the pane, since a connection that
dropped has usually just said why. The panel grows another page while a pane is
on a host: the files at the other end, in the same tree the Files page draws,
opened over the connection that is already there.

Three ways to get somewhere. `Ctrl+Shift+F` opens a find bar over the terminal:
every match on screen and in the scrollback is highlighted as you type, the
tally counts them, and Enter walks forward while Shift+Enter walks back,
scrolling each match into view. It follows the keyboard rather than the mouse —
press it while a file pane has the focus and it is that file being searched, in
GtkSourceView's own bar — and `F3` steps whichever of the two is open. Text
already selected can be handed to the bar as the term, which is the quickest
way to look for something a command just printed. `Ctrl+Shift+P` opens the command palette —
everything the window can do, by name, with the keys that do the same thing
beside it, and under that every terminal in the workspace, so a shell in another
project is one query away rather than a hunt through tabs. Holding `Ctrl` and
pressing `Tab` brings up the tab switcher: a card per tab with a picture of what
is in it, in the order they were last worked in, so one press and release goes
back to where you just were.

`Ctrl+Shift+Page Up` and `Ctrl+Shift+Page Down` walk the scrollback a command at
a time rather than a screenful at a time, putting the prompt the search lands on
at the top of the viewport. What a prompt is comes from the shell: OSC 133 is the
only thing that says where one output ends and the next command begins, and a
guess based on how a line looks would be wrong on every prompt somebody styled.
So a shell that does not send it gets a toast saying that, rather than a key that
silently does nothing. kitty, Ghostty and WezTerm all put this on `Ctrl+Shift`
and an arrow, which is project switching here, so it went one key over onto the
pair that already scrolls.

`Ctrl+Shift+R` reopens the tab just closed, and the four closed before it. It
comes back with its columns, its panes, the room they had, the name that was
typed, and what its shells had printed, because the scrollback is taken at the
moment of closing rather than looked for afterwards; the shells themselves are
gone, because they were hung up when the tab closed. It is the session snapshot's
own shape, one tab of it, which is why a reopened tab restores exactly what a
restored session does. Not `Ctrl+Shift+T`: a terminal has had that key for a new
tab since long before a browser had an undo for closing one.

A tab dragged off the strip and dropped on the desktop becomes a window of its
own, and the shells inside it keep running through the move: nothing is
restarted, nothing is re-attached. Every widget in a pane looks its window up
by the tab it belongs to at the moment a signal fires, rather than holding the
window it was built in, so handing a tab over is moving one entry per pane
between two maps and moving the model's `Tab`. Dropping a tab onto another tuni
window is not that path and closes it instead: libadwaita only asks for a new
window when the drop had no window under it. A project row dragged off the
sidebar does the same for everything in it, one transfer per tab, and its menu
says "Move to a New Window" for the same thing by name, because a drag onto the
desktop is a gesture a tiling compositor can swallow before it reaches the
window. The last project in a window may go too; what stays behind is the same
New Project screen that closing it would have left.

`Ctrl+Alt+I` sends what is typed to every pane of the tab rather than to the one
with the keyboard, and a banner across the top says so until it is pressed
again, because a mode this loud has to be visible from the pane being typed
into. Per tab rather than per window: the panes of one tab are usually the
machines being worked on together and the tab behind it is usually something
else. It is not written to the session either, since a window that comes back
typing into four shells at once is a surprise nobody asked for. A pane opened
while it is on joins in; the write each neighbor gets is the bytes the key
encoded to, not a replay of the signal, so nothing goes round the tab twice.

`Ctrl+Shift+Backspace` wipes the command being typed, from wherever the cursor
happens to be, and in every pane at once while typing is broadcast. It is
`Ctrl+U` and `Ctrl+K` in one keystroke, which is what the line actually costs:
readline's `Ctrl+U` kills backwards from the cursor and zsh binds it to the
whole line, so the pair is the only spelling that is correct in bash, zsh and
fish alike. On `Ctrl+Shift` rather than on `Ctrl`, where everything else this window
takes from the shell lives: `Ctrl+Backspace` already means a word to a reader,
and a key that deleted one word deleting the whole line instead is the kind of
surprise that costs a command. On the alternate screen it is left alone and the
key goes to the application, since those two bytes are a scroll and a digraph to
`vim` rather than a line to erase.

Closing the window writes that arrangement down, and opening it again puts it
back: the projects, their tabs, the columns and panes inside each one with the
room they had, the names that were typed, and a fresh shell in each pane's last
working directory. What those shells had printed is not restored unless it is
asked for. A second window can be opened for a scratch project on another
screen; it starts empty and never writes the session, so the arrangement that
comes back is the one the first window was left in. A settings window under
`Ctrl+,` edits the font, the two themes, the cursor, the scrollback, the shell
to run, whether a file pane wraps its long lines, and that last decision,
writing each change to `~/.config/tuni/config.toml` as it is made. It is four
pages, Appearance, Terminal, Session and Shortcuts, and each row is a title and
one short line saying what it does: a page long enough to scroll through twice
is where a setting goes to be missed, and a subtitle that argues its own design
is a setting nobody reads. There is no
OK button; a line of terminal beside the font rows is drawn in whatever has
just been chosen, since that is the only way a font or a palette is actually
picked. The family is chosen from what this machine has, asked of the font map
rather than typed: the monospaced faces first and every other family after,
since fontconfig's `spacing` is a label plenty of coding fonts are missing and a
list that hides them reads as a list of what is installed. The row says under
its title when the family picked is proportional, and when it is not installed
at all. Whatever is configured stays in the list either way, rather than the
dialog quietly selecting something else. It also says when no Nerd Font is
installed, which is the whole of why a prompt's icons come out as boxes.

| Shortcut | Action |
| --- | --- |
| `Ctrl+Shift+D` / `Ctrl+Shift+E` | Split right, split down |
| `Ctrl+Shift+W` | Close the pane — and the tab, when it was the last one |
| `Ctrl+Alt`+arrows | Move the focus one pane in that direction |
| `Ctrl+Shift+]` / `Ctrl+Shift+[` | Next pane, previous pane |
| `Ctrl+Shift+Enter` | Show the focused pane alone, and back |
| `Ctrl+Alt+Shift`+arrows | Grow or shrink the focused pane |
| `Ctrl+Alt+=` | Give every pane the same room |
| `Ctrl+Alt+I` | Type into every pane of the tab at once, and stop |
| Drag a pane's grip onto another | Move it to that pane's left, right, top, or bottom |
| Drag the gap between panes | Move the divider |
| `Ctrl+Shift+T` | New tab: a shell, or the host list when `new-tab` says so |
| `Ctrl+Shift+O` | The host list, in a tab of its own, whichever that setting says |
| `Ctrl+Shift+R` | Reopen the tab just closed, with its layout, its name and what its shells had printed |
| `Ctrl+Page Down` / `Ctrl+Page Up` | Next tab, previous tab |
| Drag a tab off the strip | A window of its own, with the shells still running in it |
| Hold `Ctrl`, press `Tab` | The tab switcher: cards for every tab, most recently used first. `Shift+Tab` walks back, `Escape` cancels, letting `Ctrl` go switches |
| `Alt+1` … `Alt+9` | Jump to a tab; `Alt+9` is the last one |
| `Ctrl+Shift+N` | New project |
| `Ctrl+Alt+Page Down` / `Ctrl+Alt+Page Up`, `Ctrl+Shift+Down` / `Ctrl+Shift+Up` | Next project, previous project |
| `Ctrl+Shift+1` … `Ctrl+Shift+9` | Jump to a project |
| Drag a project off the sidebar | A window of its own, every tab in it, with the shells still running |
| `F9` | Show or hide the sidebar |
| `Ctrl+Shift+B` | Show or hide the panel |
| `Ctrl+Shift+G` | Open the panel on the repository, or close it when it is already there |
| `Ctrl+Shift+I` | Open the panel on the session: processes and ports |
| `Ctrl+,` | Preferences |
| `Ctrl+S` | Save the file in the focused pane |
| `Ctrl+Shift+P` | The command palette |
| `Ctrl+Shift+F` | Find in the pane that has the keyboard; Enter and `Shift+Enter` walk the matches, `Escape` closes |
| `F3` / `Shift+F3` | Next match, previous match |
| `Ctrl+Shift+H` | Find and replace in the file pane that has the keyboard |
| `Ctrl+Shift+K` | Clear the terminal: the screen, the scrollback behind it, and a fresh prompt |
| `Ctrl+Shift+Backspace` | Wipe the command being typed, whatever the shell, cursor wherever it is |
| `Ctrl+F` / `Ctrl+H` | Find, find and replace — in a file pane only |
| `Ctrl+G` / `Ctrl+Shift+G` | Next match, previous match — in a file pane only |
| `Ctrl+Shift+C` / `Ctrl+Shift+V` | Copy selection, paste |
| Middle click | Paste the primary selection |
| Right click | The word under the pointer is selected, a click inside the selection keeps it, and a menu of what these keys already do opens: copy, paste, select all, find, clear, the splits, and above them the hyperlink under the pointer when there is one |
| `Ctrl+Shift+A` | Select everything, scrollback included |
| Drag, double click, triple click | Select by character, word, line. `Ctrl`+triple click selects one command's output where the shell marks its prompts. A repeat counts within half a second and one cell of the first, Ghostty's clock |
| `Ctrl+Alt`+drag | Block selection |
| `Shift`+click | Extend the selection to the click, at the granularity it was made at. While an application is tracking the mouse this is also how selecting works at all |
| `Ctrl+Shift+M` | Take the mouse back from applications entirely, and hand it over again |
| `Ctrl`+click | Open the hyperlink under the pointer |
| Drop a file on a pane | Its path, quoted, onto the prompt as an argument, followed by a space |
| `Shift+Page Up` / `Shift+Page Down` | Scroll the viewport by a page |
| `Ctrl+Shift+Page Up` / `Ctrl+Shift+Page Down` | Scroll a command at a time, where the shell marks its prompts |
| `Shift+Home` / `Shift+End` | Jump to the top of the scrollback, or the bottom |
| Wheel | Three rows a notch, pixel for pixel on a touchpad. On the alternate screen it becomes arrow keys, which is how `less` scrolls without taking the mouse |
| `Ctrl+plus` / `Ctrl+minus` / `Ctrl+0` | Font a point larger, smaller, back to the configured size |

New Window, Show Files and Use Selection for Find have no key of their own —
they are in the menu and in the palette, which is where anything without a
shortcut can be reached by name.

The window's shortcuts are a page of the settings. A row is clicked and the
new key pressed; Backspace turns a shortcut off, which leaves the action in
the menus and the palette but takes it off the keyboard, and a changed row
says what the default was and offers it back. What is configurable is exactly
the table above: the editor's own keys stay scoped to the editor so a shell
never loses them, and the numbered tab and project keys move as a family or
not at all.

Coming back to the window hands the keyboard to the pane. GTK restores whatever
had the focus when the window left, and after a click on the tab strip or a
toolbar button that is something a keypress does nothing to, which reads as a
terminal that has stopped listening. Anything that would rather keep the
keyboard keeps it: an open dialog, and every place text is typed, so a search
half entered in the find bar or a path in the Files panel survives a trip to
another window.

A pane nobody is looking at can still say something. The bell marks its tab as
wanting attention, and when the window is not the focused one it arrives as a
desktop notification named after the terminal that rang it. So does anything a
program asks for outright — OSC 9, OSC 777, and kitty's OSC 99, which is what
build tools and `notify-send`-alikes emit — and a progress report, OSC 9;4,
draws a hairline along the bottom of the pane that printed it: blue while it
runs, red when it failed, amber when it is paused, and the full width when the
program only knows that it is busy. It clears when the program says so, or
after fifteen seconds of nothing further. Each pane's notification replaces its
own rather than stacking one banner per line of output, and focusing the pane
withdraws it.

A command that ran long says so when it ends. Every two seconds a pane asks
`/proc/<shell>/stat` which process group has its keyboard, which is how a shell
answers "is something running" without being configured to; when that group goes
away after ten seconds or more, the tab takes the attention mark and a desktop
notification names the command and how long it took. It is the bell's rule about
when to speak: the pane has to be unfocused, or the window inactive, since a
banner about a build somebody just watched finish is a banner that teaches people
to turn banners off. What it cannot say is whether the command succeeded: the
foreground process group is gone by the time anyone can ask it, and the exit code
went to the shell. A shell that marks its prompts with OSC 133 knows the code and
could say so; nothing here requires that shell, so nothing here reads it.

A coding agent thinking in a pane says so in two places: a spinner on its tab,
and a spinner in place of the folder on its project's row in the sidebar. Two
depths of the same fact, so that finding the pane which is working takes no
clicking, and nothing louder than that. A bar sweeping the top of the window was
built first and thrown away: a whole animation across the chrome to say a shell
three tabs back is busy is too much of the window given to one pane's state.
Both spinners are the one libadwaita draws on a loading tab, in the slot an icon
would occupy, so the row and the tab say it in the same hand and neither changes
width when a turn starts.

When the turn ends into a tab nobody is looking at, both spinners become an
exclamation and the tab takes the same attention mark the bell raises, because
an answer waiting behind a tab is the half of this worth interrupting for.
Selecting the tab clears it, on the click that reads the answer. A turn that
ends in the tab already on screen is marked nowhere: it was watched as it
happened, and a mark to dismiss for something already seen is a mark that
teaches people to dismiss marks.

What it reads is the title, and nothing else. Claude Code writes its own state
there — `✳ Claude Code` between turns, a braille spinner frame in place of the
star while it works — so a pane reports for itself over an escape sequence the
terminal parses anyway. No process is polled for it and no screen is scraped;
the CPU an agent burns was measured first and cannot tell thinking from waiting,
which is the reason this is read rather than sampled. Braille is the whole test:
a spinner is drawn out of that block and no prompt, path or command line starts
with one. An agent that says nothing in its title simply never reports, the same
as every pane that is not running one.

The glyph then comes off the name. Having turned it into a spinner on the tab,
leaving it in the label would say the same thing twice and animate a frame under
the tab's own text four times a second, which is what makes a tab strip restless
to sit beside. The idle star comes off with it, so the tab reads `Claude Code`
either way rather than renaming itself as each turn starts and ends. It is also
what the pane compares against: an agent redrawing its spinner changes the state
and not the name, so the window's title-and-labels refresh runs when the name
moves rather than on every frame.

An image printed to a pane is drawn as an image. Tuni speaks the kitty graphics
protocol, so `timg`, `chafa -f kitty`, matplotlib's kitty backend and anything
else that transmits one lands in the pane at the size, position and stacking
order it asked for — under the text, over it, or under the cell backgrounds —
and scrolls with the text it was printed beside.

That is kero's behavior, less the one piece of it that is macOS by nature: the
Sparkle auto-updater, which a package manager stands in for. The window blur it
gets from AppKit is asked for the two ways Ghostty asks on Linux: over
`ext-background-effect-v1` on Wayland, once the manager's capabilities event
has named blur among what it renders, and as KWin's
`_KDE_NET_WM_BLUR_BEHIND_REGION` window property on X11. There is no third
way. A desktop that answers neither, GNOME first among them, offers nothing to
ask, so the switch quietly does nothing there.

Consuming a 200 MiB stream, measured with `scripts/throughput.sh` on this
machine:

| Terminal | Throughput |
| --- | --- |
| Tuni | 117 MiB/s |
| Ghostty | 98 MiB/s |
| Konsole | 45 MiB/s |

Drawing a full 120x37 viewport costs 516 µs at the median and 1.0 ms at the
95th percentile, against a 16.7 ms frame budget. That answers the question the
spike existed to answer: Pango is not the bottleneck, and there is no reason to
reach for a GPU glyph atlas.

## How it works

Ghostty's terminal emulation, not a reimplementation of it. `libghostty-vt` is
the cross-platform, zero-dependency half of Ghostty: the SIMD VT parser, the
full terminal state and grid, cell styles, reflow, scrollback, Kitty keyboard
encoding, and SGR mouse encoding. It does not do rendering or PTY management, so
those two are ours.

| Crate | Responsibility |
| --- | --- |
| `tuni-vt` | Facade over `libghostty-vt`. Nothing above this line imports it. |
| `tuni-pty` | Shell process, PTY, reader thread, window resize. |
| `tuni-core` | Portable models: config, themes, projects, panes, session, git. No GTK. |
| `tuni-gtk` | GTK4 + libadwaita widgets, window, actions, keybindings. |

The upstream C API is explicitly pre-1.0 and expected to change, which is why
the dependency is pinned to a commit and why every call to it goes through
`tuni-vt`.

Projects and tabs are a plain model in `tuni-core`, but the tab strip is an
`AdwTabView`, and a tab strip already knows things the model would otherwise
have to be taught: that a new tab belongs next to the current one, that closing
one falls to a neighbor, that a drag ends where it was dropped. So the widget is
the record of tab order and selection, and it reports one way into the model —
never the reverse. Two records that can disagree need a guard against drift;
one record needs nothing.

Panes are the other way around, for the same reason. No GTK widget implements a
niri layout — a tab is a row of columns, a column is a stack of panes, and
nothing nests deeper than that — so the model in `tuni-core` is the record and
the widget draws whatever it says. `GtkPaned` was the alternative and it is the
wrong shape: it holds two children and nests to hold more, which turns a flat
row of four into a tree of three, and dragging one divider then moves panes that
were nowhere near the pointer. Sizes are weights rather than pixels, and the
arithmetic that turns weights into a row of tiles lives in the model beside its
tests, not in a widget where nothing can reach it.

The saved session is two files rather than one. `session.json` is the shape of
the window — projects, tabs, columns, panes, weights, focus, names — and it is
small, cheap to write, and safe to keep: a directory path and a title are what
any shell already puts in the window title. `history.json` is what the
terminals had printed, and it is a different kind of thing entirely, because a
scrollback holds whatever was on screen when the window closed. So it is
written only when it has been asked for, capped at 500 lines a pane the way
kero caps it, and deleted outright the moment the setting is turned back off.
Both are written beside themselves and renamed into place, so a crash mid-write
costs the previous session rather than both. Every field the snapshot reads is
optional and every unreadable tab is skipped, which is what lets a file written
by an older build still open — and `TUNI_SESSION=0` turns the whole mechanism
off, restore and save alike.

Restoring it is off by default. A terminal that opens on one empty shell is
what everyone else's opens on, and a window that comes back with eight panes
and four projects nobody asked for on this run is a surprise rather than a
convenience; `session.restore` on is for people who want the workspace back and
have said so. The window is written down either way, so turning the setting on
restores the last one rather than starting to collect from that moment. What a
restored pane starts in is its own switch: `terminal.restore-directory` off
sends every one of them where a shell started outside the window would go, for
people who want the arrangement back without the state that came with it.

Where a shell is comes from OSC 7 while a shell sends it, and from `/proc`
while one does not. Fish reports for itself; bash and zsh report only through a
distribution's prompt hook, and every one of those hooks checks for VTE first,
so under them the sequence never arrives and every pane would have inherited
and restored nothing. The kernel knows regardless. A pane whose shell has never
sent OSC 7 reads `/proc/<pid>/cwd` instead, on a timer of the same couple of
seconds the panel polls at, and a shell that does report is believed: the first
OSC 7 stops the timer for good, as does the session ending. A timer rather than
a read on each drain, because after a `cd` the prompt is the last thing printed
and a poll riding on output would have nothing left to ride on at the moment
the answer changed.

The file tree is a flat list rather than a tree of nodes. What a list view
wants is rows, and what an expandable tree costs is a second structure that has
to be kept in step with them; a list of `(name, path, depth)` rebuilt from the
set of open directories is the same information with nothing to keep in step.
Only open directories are read, each read is sorted directories-first and then
naturally, so `file10` follows `file9`, and depth is capped at 32 so a symlink
that points at its own parent runs out rather than forever. `.git` is hidden
and every other dotfile is shown, dimmed — kero hides only `.git` too, and a
file tree that disagrees with `ls` is worse than one that shows build output.
The disk is re-read every two seconds instead of watched with inotify, which is
what kero does: a watch per open directory costs a descriptor and a debounce,
and a cached directory read costs nothing measurable. The panel redraws only
when the rows actually differ, so most of those reads change nothing on screen.
Deleting is `GFile.trash`, which is recoverable, and revealing a file goes
through `GtkFileLauncher` so it reaches the portal in a sandbox and the session
bus outside one. Whether the panel was showing, which page it was on, and how
wide it was dragged are part of `session.json`.

The repository is read by running `git`, not by linking a library, because the
question the panel answers is what the command line would say — the same
config, the same hooks, the same includes, the same worktree rules. Every one of
those processes runs off the main loop and comes back as a future, with a
generation number attached: a status that arrives after the shell has `cd`'d
into another repository is dropped rather than drawn. Reads carry
`GIT_OPTIONAL_LOCKS=0` so watching a repository cannot fight a build for the
index lock, `GIT_TERMINAL_PROMPT=0` so a fetch that wants a password fails
instead of hanging on a terminal that is not there, and `LC_ALL=C` because the
output is parsed. Status is `--porcelain=v2 -z`, which is the format that
survives a filename with a newline in it. Only one action runs at a time; the
working tree is re-read from scratch afterwards, since a commit moves the
history and a checkout moves everything. What that model decides — whether a
commit is possible, which paths a discard has to restore and which it has to
delete — lives in `tuni-core` with tests against a real repository, and the
widget only draws the answer.

What Info knows, it reads out of `/proc` rather than out of `ps` and `lsof`.
Both of those read `/proc` themselves, `lsof` is frequently not installed, and a
pair of processes every two seconds is a cost worth not paying. The shell's own
children are found by walking every `/proc/<pid>/stat` once and breadth-first
descending from the shell's process id, which also catches a grandchild — the
`node` a package script started. A listening port is `/proc/net/tcp` and
`/proc/net/tcp6` filtered to state `0A`, which yields socket inodes; those are
matched against the `socket:[…]` links in each process's `fd` directory, which
is what turns a port into the process holding it. CPU is the lifetime average
`ps` reports rather than an instantaneous sample: ticks over `_SC_CLK_TCK` over
how long the process has been alive. All of that parsing is pure functions over
the exact text a kernel writes, tested in `tuni-core` without a process tree to
point them at, and the reads happen on a worker thread with the same generation
stamp the git panel uses.

The agent numbers are read the same way, off files the agent wrote for its own
purposes. Claude Code and Codex append a line of JSON per turn; OpenCode keeps a
row per session in SQLite, opened read-only so that a database being written to
is safe to read. Which of those sessions belongs to the pane is settled by the
working directory: Claude Code puts it in the transcript directory's name, Codex
records it in the first line of a rollout, OpenCode stores it in a column. A
transcript grows to tens of megabytes over an afternoon and the page polls every
couple of seconds, so the reader remembers where in each file it stopped and what
the file came to by then; a poll costs the bytes appended since the last one,
which is milliseconds against a first read's hundred. A line that is half written
when the read reaches it is left for next time, because the agent is appending
while this reads. Claude Code writes several lines per reply and repeats the
usage on a retry, so a request id already counted is counted once.

The editor's keys belong to the pane rather than to the window, and that is the
whole reason it can share a window with terminals: `Ctrl+S` is flow control to a
shell and `Ctrl+F` is a page forward in `less`, so binding either one globally
would break the pane next door. They are a shortcut controller on the editor
widget, scoped locally, which means they exist exactly while a file has the
keyboard. Saving writes a neighbouring file and renames it onto the target,
which is atomic on every filesystem Linux ships: an interrupted save costs the
new text, never the old. Two things follow from renaming rather than truncating
and both are handled — a symlink is resolved first, so it is still a symlink
afterwards, and the old file's permissions are copied onto the new one, so a
script stays executable. What the session remembers is the cursor offset and
nothing else. Unsaved text is not written to a cache to be recovered later; what
is on disk is the file, and the question before closing is asked while there is
still someone there to answer it. A file is text if it decodes as UTF-8 and
binary if it does not, which is a decision the bytes make rather than the
extension.

A diff is `git diff` and its output parsed, for the same reason the panel is:
what the pane shows has to be what the command line would print, renames and
`diff.algorithm` and all. An untracked file has nothing in the index to compare
against, so it is `--no-index` against `/dev/null`, which is also the one form
of diff that answers with an exit status of one when it worked. Staging is the
hunk as it was parsed, handed back to `git apply --cached` on standard input,
and unstaging is that same patch with git's own `--reverse` — a patch written
backwards by hand is how a staging tool loses a line. Within a changed line the
words that moved come from `similar`, run only on pairs of lines that a hunk
already puts opposite each other; a diff of a generated file draws 4000 lines
and then says how much it left. The pane re-reads on the same two-second timer
as the panel, so a change made by the shell beside it appears without asking,
and a read that comes back byte-for-byte identical is dropped rather than
redrawn, which is what keeps the scroll position under someone reading.

The SSH client drives OpenSSH instead of speaking the protocol, for the reason
the git panel runs `git`: what tuni shows has to be what the command line would
do. So what an alias means is a question for `ssh -G`, which prints the
configuration a real connection would use with `Match`, `Include`, wildcard
blocks and canonicalisation already applied. Tuni's own reader of
`~/.ssh/config` therefore only enumerates the names a list can show; it never
has to agree with ssh about what one of them resolves to. One caveat has teeth:
`ssh -G` runs the user's `Match exec` commands, so it is asked when a host is
opened, edited or looked at, and never on a timer or for a whole list at once.

Connecting is `ssh` running in a pane, and that is the design rather than an
implementation detail. Everything an authentication can involve is interactive:
a password, a passphrase, a push notification, a touch on a hardware key, the
question about an unknown host's key, a password that expired this morning. A
terminal is the one thing in this window that answers an interactive prompt
correctly, and it already exists. So tuni holds no secret because it never
receives one, and there is nothing to leak, sync or store. The alternative,
watching a hidden pty for output that looks prompt-shaped and popping a text
field over it, is a keylogger with a guess in front of it: it cannot tell a
password prompt from a banner, and it would put the password in tuni's address
space, which is the one place this feature is built to keep it out of.
Everything tuni runs that is not a pane carries `BatchMode=yes`, the same
sentence as the git panel's `GIT_TERMINAL_PROMPT=0`, plus
`SSH_ASKPASS_REQUIRE=never` so an askpass inherited from the environment cannot
open a dialog out of a background process.

Panes on one host share one connection, since a second tab asking for a second
2FA code is what makes a client tiring. That is OpenSSH multiplexing, and the
master is not tuni's child: it is the first pane's own `ssh`, left running by
`ControlPersist` for everything after it to attach to. The socket goes under
`$XDG_RUNTIME_DIR/tuni/ssh`, which is tmpfs, 0700 and emptied at logout, so a
socket left by a killed master cannot outlive the machine's uptime;
`~/.cache/tuni/ssh` is the fallback, and it is the whole reason startup sweeps
for sockets nothing answers on. Sharing is arranged with the shortest set of
options that makes it work, because every option tuni adds overrides one
somebody set on purpose, and a host whose own configuration already sets
`ControlPath` gets none of them at all: overriding a working `ControlMaster`
would put tuni's connection and the `ssh prod` typed in the next pane on two
separate logins. The exception on merit is `ServerAliveInterval`, without which
a suspended laptop's connection hangs for the kernel's retransmit timeout,
about fifteen minutes, and every pane on that host reads as frozen rather than
disconnected.

What the connection outlives is the part worth knowing. `ControlPersist` means
the master is still there after the pane closed, so the last window out hangs
up every master tuni started, on the way through the close and on that thread,
because a thread detached there dies with the process. The ones the user's own
configuration owns are never hung up, since a session tuni knows nothing about
may be sitting in one. A `SIGKILL` leaves up to `ssh-control-persist` seconds
of connection behind, which is bounded and swept at the next start; a cleanup
handler that cannot run would not have done better. Inside a Flatpak the
sandbox has an `$XDG_RUNTIME_DIR` of its own, so those sockets are invisible to
an `ssh` typed outside it, and the sharing stops at the sandbox boundary.

Tuni owns one file and writes one line into another. The line is an `Include`
of `~/.config/tuni/ssh/hosts.conf`, placed at the top because ssh keeps the
first value it obtains for a keyword and an early `Host *` block would swallow
an include appended to the end, and `~/.ssh/config` is copied to
`config.tuni-backup` before it is ever touched. There is no setting for
whether tuni may write it: without that line the hosts tuni saves would be
invisible to the `ssh` it runs, so the switch would only be a switch for
half-breaking the feature, and a backup plus one self-explaining line is the
better answer. `hosts.conf` is rewritten whole from the model, at 0600 from the
moment it exists, and a host declared in the user's own file is read, listed and
connected to but never rewritten: that file has includes, first-value-wins
ordering and comments people rely on, it is frequently a symlink into a
dotfiles repository, and a terminal that reformats it loses trust once and
permanently. Two guards sit in front of the rename. The new file is put to
`ssh -F` first, which exits 255 on a keyword it does not know and touches no
network, because a broken include breaks ssh for every script on the machine.
And a value that could start a line of its own is refused rather than written,
since our file is included into theirs and a newline inside a value is an
arbitrary keyword, `ProxyCommand` among them. What ssh syntax cannot express,
a label, tags, when a host was last opened, lives in
`~/.config/tuni/ssh/meta.json` keyed by alias, which is what lets a hand-written
host carry them without tuni putting a byte in the file that declares it.

A forwarded port is two different things under one name. A `LocalForward` in the
host's own block belongs to ssh, which brings it up with the connection, and
there is nothing in the window that could start or stop it. One kept in
`meta.json` is tuni's, opened and closed against a running master with
`ssh -O forward` and `-O cancel`, which is what the switch in the session
inspector does. Whether it is up is not a question the mux client answers, so a
local or dynamic forward is confirmed by a listening socket in `/proc/net/tcp`,
found by the same reader the Ports section already runs. A remote forward cannot
be confirmed from this machine at all; what ssh said when it was asked is all
there is, and the row claims no more than that. Asking for port zero has the far
end pick one and ssh prints the number, which is worth remembering because a
cancel has to be spelled the way the master recorded the request. Before any of
it, the port is tried here by binding it, so a clash names the process holding
it rather than returning the mux client's error, which nobody can act on. That
is a race, and the real failure path is still there: the check only makes the
ordinary case readable.

Snippets are a file of the same kind, `{name, body}` in
`~/.config/tuni/ssh/snippets.json`, offered by name in the command palette. One
is typed into the pane that has the keyboard rather than run behind it, so it
works against whatever shell is there, on this machine or the far end, and what
ran is on the screen where the person who ran it can read it. The last character
carries the rest of the meaning: a body ending in a newline runs, one that does
not lands on the prompt to be finished by hand, which is how a snippet that
would drop a table can be kept without a mis-hit Return being able to fire it. A
host may name one to have typed when it connects, and the waiting in front of
that is the whole of it. Sent too early the text goes wherever the login went,
and a password prompt is one of the places that could be, so tuni watches for
the control socket, which ssh binds only after it has authenticated, and then
asks the master once whether it is answering. A host whose configuration shares
nothing has no such moment to point at and gets nothing typed.

Keys are read and never held. The list under the launcher's menu is `~/.ssh`
scanned for `.pub` files, one `ssh-keygen -l` each for the type, the size, the
comment and the fingerprint, and one `ssh-add -l` for what the agent has, which
answers the only question a key raises day to day: whether connecting with it
will ask for a passphrase. That call tells three things apart and the row says
which, since no agent at all and an agent holding nothing lead to different
advice. Everything that changes something goes out as a command typed onto the
prompt of a shell of its own: `ssh-keygen` to make a key, `ssh-copy-id` to put
one on a host, `ssh-add` to hand one over. Each of them asks for a passphrase or
a password sooner or later, and a dialog that collected the answer would be
storing a secret, which is the one thing tuni does not do. Typed and not run,
like a snippet without its newline, because a command about a private key is
worth reading before it goes anywhere. Deleting a key, changing a passphrase and
managing the agent are all missing on purpose: `ssh-agent` and the desktop's
keyring do that job already, and a wrong button in this window would be a
security bug rather than a bad row.

The files on the far machine are a fourth panel page, on the switcher while the
pane holding the keyboard is on a host and off it the moment that pane is not,
under a server icon rather than a second folder, because two folders a small
badge apart are two pages nobody tells apart at sixteen pixels. What fills it is
a small client speaking version 3 of the SFTP protocol over
`ssh -s <host> sftp`, which on a live master is one more channel on the
connection the panes already have, so nothing authenticates twice, and
`BatchMode=yes` makes sure a file listing is never the thing that asks for a
password. The rows come from the tree the Files page already has: `Tree` reads
through a `Directory`, which is the disk on one page and, on the other, what has
so far come back. That indirection is the whole of the rule that nothing remote
touches the main loop. A directory nobody has asked about yet reads as empty and
a request goes out on a worker thread, the rows appear when the answer lands, and
an answer about a host that is no longer the one on screen is dropped by a
counter rather than cancelled, since it costs nothing to throw away. A remote
directory announces nothing when it changes either, so this page stays out of the
panel's two-second poll and reads on navigation and when asked. A symlink pays
for a second round trip to learn whether it points at a directory, which is the
trade the local tree makes for the same reason and for the same one entry in a
hundred. Getting about is the local page's gestures over remote paths: the
chevron opens a directory where it stands, a double click makes it the root, the
header takes a typed path, and the buttons under the thumb walk the history,
which empties with the session since every path in it names a machine this page
has left.

Files move one at a time. A download is written beside where it is going under a
dotted name and renamed onto it once it is whole, the discipline the editor
already saves with, so an interrupted transfer costs the copy rather than
leaving half a file under the name something else would open. An upload arrives
with the permission bits it has here, which is what the `sftp` program does too:
a script that runs on this machine runs on the other one. How far it has got is
counted rather than guessed, since this drives the loop instead of reading
another program's output, and the number reaches the progress bar through two
integers a worker writes and a local timer reads, which is a timer over memory
and not a poll of anything remote. What it is not is quick: one request is
outstanding at a time, so a file crosses at one round trip per 32 KiB, roughly
640 KB/s on a 50 ms link. The protocol's request ids exist so that sixty-four
can be in the air at once and that is a later commit. A second transfer waits
behind the first in a queue the bar counts down, since one pipe carries one file
and a request blocked on a lock is the thing with nothing on screen to say so. A
failure empties that queue rather than trying the rest: whatever the far end
refused this file for is what the ones behind it were about to meet, and the
dialog says how many were left where they are.

Files dragged onto the page go up. They land in the directory being shown, or in
the one whose row they were dropped on, and they queue like any other transfer.
A folder is refused, and the refusal says what to reach for instead: there is no
recursive copy here, and one written on top of a single outstanding request
would lose to `rsync` at the job `rsync` exists for.

Making a directory, renaming and deleting are the rest of it. Deleting asks
first, and the question says that there is no trash on the other machine, since
nothing here can put one there and a dialog that implies otherwise is a lie a
person only finds out about once. A directory goes only when it is empty, which
is the far end's rule and not this one's. Every change re-reads the directory it
happened in, because nothing over there will say what changed. What version 3
will not say is why it refused: OpenSSH's server answers a name that is taken and
a directory that is not empty with the same bare failure, whose text is the word
"Failure". So a refusal spends one more round trip asking the question the code
should have answered, and the dialog says that something is already there with
that name, or that the directory still has something in it, rather than repeating
the far end's least useful word. That costs nothing until something goes wrong.

sshfs would have been free: mount the host, and every page in this window works
on it unchanged, the editor and the git panel included. It is the worst idea
available. The panel polls on the main thread, so each tick becomes a round trip
per open directory, and when the link drops FUSE reads block uninterruptibly,
which means the main loop is gone, `SIGTERM` does not help, and the way out is a
`fusermount -u` typed into a terminal that is inside the frozen window. Upstream
archived it, and it does not work in a Flatpak. `ssh host ls` needs a shell at
the far end, so it fails on the sftp-only accounts and appliances a file browser
is most wanted for. The `sftp` program driven by `-b -` was the realistic
alternative and it loses on names: its listing cannot represent a file whose name
holds a newline, `ls` glob-expands its argument with no way to stop it, so a
directory called `report[2024]` is unreachable and one called `*` lists its
parent, and a single failed command ends the batch. The protocol underneath has
no quoting layer, no globber and no locale. Names are bytes, attributes are
integers, and a failure is a status code on one request rather than the end of
the session.

An SSH library was the obvious alternative and it is ruled out by the same
constraint everything else here follows. `russh` and `libssh2` bring their own
authentication, so keys, passphrases and keyboard-interactive answers would
land in this process, which is precisely what running `ssh` avoids. They also
carry their own reading of `ssh_config`, `known_hosts`, the agent protocol and
`ProxyJump`, each of them subtly different from the `ssh` in the pane beside
them, so an alias could resolve to one machine in the window and another on the
command line. And they cannot attach to a `ControlMaster`, which is what the
rest of this is built on.

Ghostty's theme catalog is vendored under `data/themes` and baked into the
binary at build time, so a fresh checkout runs with all 574 and a packaged
build needs no data directory beside the executable. The same theme drives the
window: libadwaita builds its stylesheet out of named colors, so overriding
those recolors the header bar and dialogs along with the terminal.

Both header bars drop the `icon` element out of whatever decoration layout the
desktop asks for, and follow the setting so a later change keeps dropping it.
KDE's default layout puts one there, and the window icon it draws is not a
button. It is a picture of the application that answers nothing when clicked,
and that shows the theme's missing-icon glyph on any machine running the binary
without having installed its icon.

Text is drawn with Pango rather than a GPU glyph atlas. Pango brings fontconfig
fallback, subpixel antialiasing, and input methods, and text quality is what a
terminal is judged on.

The cell width is measured from real glyphs rather than read off the font's own
`approximate_char_width` hint — the widest printable ASCII character, because a
face that calls itself monospace is not obliged to prove it — and it is rounded
to whole pixels, with glyph positions rounded to match. Both are what keeps the
eightieth column drawn where the eightieth background was filled. Every run is
placed on the row's baseline rather than in its own box, so a character that
falls back to another face lands on its column and on the line instead of near
them. Runs of plain ASCII share one layout; anything wide, combining, or
borrowed from a fallback face gets a layout of its own. Ligatures are off by
default, which is a deliberate divergence from Ghostty and kero: a ligature is
one glyph where the terminal still counts several cells. `TUNI_LIGATURES=1`
turns them back on.

Box drawing and block characters, U+2500 to U+259F, are the exception: they are
drawn from the cell's own measurements rather than taken from the font. A
designer's `█` is as tall as it was drawn and the cell is as tall as the line
height says, so a font's answer leaves a column of blocks striped and a frame
with holes at its corners. Ghostty calls its version of this the sprite font,
and the geometry here is that one ported: light lines at the face's underline
thickness, heavy at twice it, arms that stop level with whatever crosses them,
and the rounded corners stroked as curves.

A letter key is named by where it is rather than by what it types: the hardware
keycode a GDK event carries is the XKB one, which is the evdev scancode plus
eight on Wayland and X11 alike, and that is what the Kitty keyboard protocol
reports. The table is Ghostty's own, so the key where a US keyboard has Q is
`KeyQ` on an AZERTY layout too, and `Ctrl+Shift+C` is under the same finger on
every layout. Every other key is named by what the keymap says it is, because
that is the only way `caps:swapescape` and its like can work at all — XKB grants
them by handing out a different keyval for the same scancode. The line between
the two is the W3C's writing system keys, which is where Ghostty draws it. What
a key types with nothing held down travels with the event as well, so `Ctrl`+`С`
on a Cyrillic layout arrives as `Ctrl+C`, and the modifiers the keymap already
folded into the character are the ones GDK says it consumed rather than a guess
at Shift.

An application that asks for the mouse gets it the moment it asks: a press is
reported as it happens, drags and all, which is what Ghostty hands a tracking
application and what lets vim paint a visual selection by mouse. Shift is how
the person at the keyboard takes the mouse back, and a Shift press or drag
selects under any tracking mode, unless the application sent XTSHIFTESCAPE to
say it wants Shift too. That is the bargain Ghostty's default
`mouse-shift-capture` strikes. Upstream parses the sequence into a flag its C
API never shows, so the facade reads the stream for it alongside the parser,
the way it already reads for notifications it swallows.

There is no reading of a drag that serves both sides: it is either the
application's or it is a selection. Shift is one answer and `mouse-reporting`
is the other, the same key Ghostty uses for the same setting. Turned off, no
application is given the mouse however loudly it asks, and every drag selects.
`Ctrl+Shift+M` turns it off and on again, because whether a program should
have the mouse is a thing that changes several times an hour.

The wheel is Ghostty's arithmetic. Movement is normalized to pixels (a
touchpad's deltas tenfold, a wheel notch worth three rows of them),
accumulated, and spent a whole row at a time with the remainder kept, so slow
touchpad scrolling arrives eventually instead of never. A tracking
application hears the result as buttons, 64 and 65 upright and 66 and 67
sideways, one press per row; the wheel is the one place Shift does not take
the mouse back, because Ghostty's does not either. On the alternate screen,
where there is no scrollback for a viewport to move over, mode 1007 turns the
wheel into arrow keys for everything else, which is how `less` scrolls
without asking for the mouse, and the arrows honor DECCKM the way the real
keys do.

The rest of the click grammar is Ghostty's as well. A repeat click counts
within half a second and one cell of the first. A third click with Ctrl held
selects one command's output, reaching as far as the shell marks its prompts
with OSC 133. A right click selects the word under the pointer before its
menu opens, and keeps a selection it landed inside. A drag pinned against the
top or bottom edge scrolls a row toward the pointer every few frames until it
comes back. And typing clears the selection, as Ghostty's
`selection-clear-on-typing` default does: the reply to the key is about to
repaint what was selected anyway.

A copy the eye cannot see is confirmed instead. Releasing a drag shows its
selection, so it announces nothing; the Copy action, the menu's Copy Link and
an application writing the clipboard through OSC 52 change something
invisible, and each raises the three-second "Copied to clipboard" toast
Ghostty raises. OSC 52 is the reason rather than a courtesy: the one copy the
person at the keyboard did not perform is the one most worth announcing, and
an empty write reads "Cleared clipboard", so a program blanking the clipboard
is exactly as visible as one filling it.

A drag that ends on a pane is not a drag the shell can be told about, so it
becomes text. Every file in it arrives as a quoted shell word with a space after
it, which is the one reading that works whatever is waiting: a prompt with a
command half typed, an editor, a program reading a filename. Uploading it would
be a guess about what the shell is, and running it would be a guess about what
the person meant. It goes in bracketed, like every other paste, so a name with a
newline in it cannot run itself. A drag carrying text rather than files is that
text, and one carrying a file the other side only holds in memory is its URI,
which is the only name it has.

The keyboard arriving and leaving is news as well, to the programs that asked
for it with mode 1004: an editor rereads a file that changed while it was away,
a multiplexer stops drawing a cursor nobody is typing at. A pane is a window of
its own here, the way Ghostty reports per surface, so handing the keyboard to
the split beside it is a departure for the pane that had it.

A hyperlink is whatever the program holding the PTY says it is, which over ssh
is not the person at the keyboard. So `Ctrl` has to be held before one lights up
at all, exactly `Ctrl`, since `Ctrl+Alt` is a block selection on its way; the
press only opens on release and only if the same link is still underneath, and
the URI is handed to the desktop only when it carries no control character and
its scheme is one of `http`, `https`, `mailto`, `ftp`, `ftps`, or a `file://`
that names this machine.

A URL that is only text gets the same treatment. A cell with no OSC 8 on it is
matched against Ghostty's URL regex, ported scheme branch for scheme branch,
run over the whole soft-wrapped line so an address broken across rows still
reads as one. Only the scheme branch: Ghostty also matches bare file paths, on
heuristics its own comments call breakable, and a path in this workspace has
better ways to open than a guess. A match whose scheme the opener above would
refuse never lights up, because an underline is a promise the click keeps.

Notifications are read out of the PTY stream a second time rather than out of
the terminal state, because at the pinned commit `libghostty-vt` recognizes
OSC 9, OSC 99 and OSC 777 without handing their payloads back — the callbacks
it offers are the title, the working directory, the bell, and the clipboard.
So `tuni-vt` runs a small state machine over the same bytes it feeds the
parser, and everything it learns arrives as part of the same `Effects` the rest
of a write produces. It is deliberately incurious: a DCS, APC, PM or SOS body
is skipped whole, so a tmux passing an inner program's OSC 9 through to the
outer terminal cannot ring the window it is running in, and a payload is
abandoned past 8 KiB so a program printing an escape it never terminates costs
nothing to ignore.

Images are the half of the kitty graphics protocol the library leaves to the
embedder: it parses the transmissions, stores the images and works out where
each placement belongs, and putting pixels on a surface is ours. So every frame
asks it for the placements — geometry alone, recomputed each time because
scrolling moves an image without touching the storage — and uploads a
`GdkTexture` only when the sixteen most recent miss. The cache key is the image
id together with the storage's generation stamp, so a plot retransmitted under
the same id is a new texture rather than the previous frame's. Drawing happens
in three passes because the protocol defines three layers, and a placement's
`z` says which: under the cell backgrounds, under the text, or over everything.
Two decisions are ours rather than the library's, since it makes neither by
default: images are capped at 64 MiB a terminal, oldest evicted first, and PNG —
which is what nearly every program transmits — is decoded by a callback that
this crate installs, with a ceiling on the decoded size, because the bytes come
from whatever is on the other end of the PTY. Virtual placements, kitty's
unicode-placeholder form, are stored and skipped: the C API reports no position
for one, and guessing is worse than not drawing.

## Installing

`make install` puts down the binary, the desktop entry, the AppStream data and
the icons; nothing else is needed at runtime, since the themes are baked into
the executable.

```sh
sudo make install PREFIX=/usr
```

`make install-data` is the same thing without the binary, for a Tuni run out of
the build directory: the window names its icon rather than carrying one, so the
desktop draws a fallback until the entry and the icon are somewhere the
compositor looks — `make install-data PREFIX=$HOME/.local`.

`packaging/` holds a Flatpak manifest and an RPM spec that both call into that
same target, and [packaging/README.md](../packaging/README.md) says what each one
needs — mainly Zig 0.15.2 and, unless it is given the offline paths, network
during the build. `make zig` fetches that Zig into `~/.local`, because every
distribution's package has moved past the version the pinned Ghostty commit
builds with.

Zig 0.16 is the other half of that pin, not a separate problem: Ghostty's main
branch requires it and the pinned commit rejects it, so building with the Zig a
current distribution ships means building against a newer Ghostty as well.
`make build-next` does exactly that — the 0.16 toolchain beside the 0.15.2 one,
a shallow checkout of the Ghostty commit this tree is tested against, and
`GHOSTTY_SOURCE_DIR` pointing the build at it. It is a second known-good pair,
which is why both are commits rather than branches; releases are built from the
0.15.2 one, since that is what the checked-in bindings were generated from.

### Updating

No package repository carries Tuni yet: no COPR, no AUR, no PPA. A release
therefore does not arrive with the rest of a system's updates, and an installed
copy would stay the version it was installed at forever. `scripts/install.sh`
stands in for the package repository that does not exist:

```sh
curl -fsSL https://raw.githubusercontent.com/unisic/tuni/main/scripts/install.sh | bash
```

A pipe rather than `bash <(curl ...)`: fish has no process substitution, and
the pipe takes nothing away, because every key the menu reads is read from
`/dev/tty` rather than from standard input.

It asks the GitHub release page what the newest version is, compares it with
what the package manager says is installed, and installs the asset built for
this distribution — the RPM on Fedora, the deb on Debian and Ubuntu, the
`pkg.tar.zst` on Arch. Installing over an older copy is what an update is, so
there is one code path rather than two. A Flatpak is left alone, because a
sandbox cannot install packages on the host.

Run with no arguments it opens a menu on the terminal's alternate screen, the
way `top` does, and leaving it puts back the command that was typed. The person
running an installer is at a terminal by definition, and a menu is what lets it
say which version is installed before anything happens, offer an older release
from the same page, or remove Tuni again — three things a bare prompt would
have to become three flags to reach. The whole run happens in that one window:
the menu morphs between the main view, the version list and the remove view
rather than scrolling a transcript, and a full redraw on every key means a list
that changes length cannot leave debris behind. Removing with settings is the
only irreversible choice, so it is confirmed on the normal screen, where the
question survives long enough to read; the rest is confirmed by `sudo` asking
for a password.

The flags are for everything that is not a person: `-y` installs or updates
without a menu, which is what the Update button runs, and `--check` reports
both versions and exits 10 when an update is available.

The window does the asking, once per run, and offers the Update button that
runs the installer in a tab of its own. That is where it belongs: every route
here writes to `/usr`, `sudo` therefore wants a password, and a terminal is the
one program in a position to offer it somewhere to ask — the same reason
`ssh-keygen` gets a pane instead of a dialog. It is also why there is no
background timer: a timer has nobody to ask for the password, so the only thing
it could automate is the half that costs nothing, which the window already
does.

The check is one request per run to a public release page and sends nothing
about the machine, but it is still a program reaching the network on its own,
so `check-updates = false` turns it off and the menu's "Check for Updates"
keeps working. `TUNI_UPDATE_API` points both halves at another release
document, including a `file://` one, which is how the dialog is exercised
without waiting for a release.

## Configuration

`~/.config/tuni/config.toml`, written by the settings window and readable
without it. Only what differs from a default is written, so an empty file and a
missing one mean the same thing. The names are Ghostty's where Ghostty has one.

| Key | Default | Meaning |
| --- | --- | --- |
| `theme` | `"system"` | `system`, `light`, or `dark` — which of the two themes below is in use |
| `theme-light` | `"GitHub Light Default"` | Any of the 574 bundled themes |
| `theme-dark` | `"GitHub Dark Default"` | |
| `font-family` | `"JetBrains Mono"` | |
| `font-size` | `11` | Points |
| `font-ligatures` | `false` | |
| `line-height` | `0` | Extra pixels between rows |
| `cursor-blink` | `true` | And then only if the desktop blinks its own cursor, and only while the program running has no opinion |
| `cursor-style` | `"block"` | `block`, `bar`, `underline`, or `block_hollow`, the shape until the program running asks for another |
| `background-opacity` | `1` | How much of the desktop shows through the window, down to `0.2` |
| `background-blur` | `false` | Whether the compositor blurs what shows through: `ext-background-effect-v1` on Wayland, the `_KDE_NET_WM_BLUR_BEHIND_REGION` property on X11. A number is read as Ghostty's blur intensity, which KWin decides for itself: any of them above zero means the same thing here |
| `window-padding-x` | `0` | Pixels of nothing between the sides of the window and the grid, up to `40` |
| `window-padding-y` | `0` | The same above and below |
| `copy-on-select` | `false` | Whether releasing a selection puts it on the clipboard |
| `mouse-reporting` | `true` | Whether a program that asks for the mouse gets it. Off keeps every drag for selecting |
| `bell` | `true` | Whether `\a` rings the desktop's own bell |
| `command` | `""` | The shell to run; empty means the login shell |
| `terminal.scrollback-lines` | `10000` | Lines kept above the screen |
| `session.restore` | `false` | Whether the window opens on the workspace the last one closed with. Off opens one empty project and one shell; the session is written down either way, so turning it on restores the last window rather than the next one |
| `terminal.restore-directory` | `true` | Whether a restored pane starts in the directory its shell was in. Only read when the session is restored at all |
| `terminal.restore-history` | `false` | Whether a restored pane replays what it had printed |
| `editor.wrap-lines` | `false` | Whether a file pane folds a long line rather than scrolling sideways |
| `window.auto-hide-tab-bar` | `false` | Whether the tab bar goes away while a window has one tab |
| `new-tab` | `"shell"` | `shell` or `hosts`: what Ctrl+Shift+T opens. Ctrl+Shift+O opens the host list either way |
| `new-tab-inherit-directory` | `true` | Whether a new tab or split starts where the visible shell is. Off opens every one where a shell started outside the window would; one project is taken out of it from its own menu instead |
| `ssh-term` | `"xterm-256color"` | What an ssh pane calls itself at the far end. Tuni's own `TERM` describes terminfo that is on this machine and not necessarily on the other one |
| `ssh-share-connections` | `true` | Whether every pane on a host goes through one authenticated connection. Ignored for a host whose own configuration already sets `ControlPath`, which tuni adopts rather than overrides |
| `ssh-control-persist` | `600` | Seconds a shared connection outlives the last pane using it, up to a day. Zero closes it with the pane |
| `ssh-reconnect-on-restore` | `false` | Whether a restored ssh pane dials with nothing to attach to. Off, so a window putting eight panes back cannot start eight logins |
| `check-updates` | `true` | Whether the window asks the GitHub release page for a newer version, once per run. Off leaves the menu's "Check for Updates" to do it by hand |
| `panel.files`, `panel.git`, `panel.info`, `panel.debug` | `true` | Which pages the panel's switcher offers. The Remote page manages itself: it exists while a pane is on a host |
| `key.<action>` | | A window shortcut changed from its default, in GTK accelerator spelling: `key.win.palette = "<Ctrl><Shift>p"`. An empty string turns the shortcut off |

A Ghostty configuration can be pasted in whole. Values do not need the quotes
TOML would want, `theme` may name a theme instead of an appearance, in either
the `theme = Catppuccin Mocha` or the `theme = light:a,dark:b` form, and a key
Ghostty repeats for its fallbacks, `font-family`, is read from the first one.
`cursor-style-blink` is read as `cursor-blink`, and `copy-on-select =
clipboard` as `true`. Keys for what tuni does not do are ignored rather than
refused, so what is left of the file keeps working.

The session itself lives under `~/.local/share/tuni`. A file pane comes back
holding the file it held, with the cursor where it was left; unsaved edits are
not part of that, since a file is what is on disk.

## Building

Requires a Rust toolchain, GTK4, libadwaita and SQLite development headers, and
Zig, since `libghostty-vt` is Zig source that is compiled during the build.

Fedora ships Zig 0.16, which is too new; fetch 0.15.2 from ziglang.org and put
it on `PATH`.

```sh
sudo dnf install cargo rust gcc make git-core curl tar xz \
    gtk4-devel libadwaita-devel gtksourceview5-devel sqlite-devel
make zig

cargo run --release
```

The same list CI installs, which is what makes it the tested one. `curl`, `tar`
and `xz` are there for `make zig` rather than for the compiler, and `git`
because `libghostty-vt` is a git dependency pinned to a revision, which cargo
clones rather than downloads.

`make check` runs what CI runs — `cargo fmt --check`, `cargo clippy
--all-targets -- -D warnings`, `cargo test`, then `desktop-file-validate` and
`appstreamcli` over the two data files — and wants four packages that a build
does not:

```sh
sudo dnf install rustfmt clippy desktop-file-utils appstream
```

Running it before a push is the point: clippy's `-D warnings` and the format
check fail the build for a nested `if` or a line rustfmt would wrap, neither of
which a plain `cargo build` says a word about.

Debugging aids, all off unless set, and none of them written back to the
configuration file: `TUNI_THEME` names one of the bundled themes for the run
and `TUNI_FONT` a font the way Pango writes one (`"JetBrains Mono 13"`), with
`TUNI_LIGATURES=1` to let them fire; `TUNI_SESSION=0` neither restores the
saved session nor overwrites it; `TUNI_DEBUG_FRAME_TIME` prints draw-time
percentiles; `TUNI_DEBUG_STARTUP` prints how many milliseconds each phase
before the first frame took;
`TUNI_DEBUG_PTY_WRITE` logs what the terminal answers back to the shell;
`TUNI_PARTIAL_REDRAW=1` puts GTK's damage tracking back on a translucent
window, where it is turned off because transparent chrome never covers a pixel
the tracking missed; and `TUNI_CAPTURE_PNG` renders the widget to a file and
exits — useful on compositors with no screenshot protocol.

## License

GPL-3.0-or-later. See [LICENSE](../LICENSE).
