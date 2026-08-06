# RPM spec, for Fedora and everything that follows it.
#
# The build fetches: cargo dependencies, and the pinned Ghostty source that
# libghostty-vt is compiled from. That rules out Koji, which builds offline —
# this is for COPR with network enabled, or for a local `rpmbuild`. Set
# GHOSTTY_SOURCE_DIR to a checkout of the pinned commit and CARGO_HOME to a
# populated registry to build it without either.
#
#   make -C .. dist   # or: git archive --prefix=tuni-1.5.1/ -o tuni-1.5.1.tar.gz HEAD
#   rpmbuild -ba packaging/tuni.spec

%global app_id dev.unisic.Tuni
# Zig 0.15.2, which is what the pinned Ghostty source builds with. Fedora's own
# zig package moves faster than that, so the build uses whatever is on PATH and
# falls back to fetching this one.
%global zig_version 0.15.2

Name:           tuni
Version:        1.5.1
Release:        1%{?dist}
Summary:        Terminals, projects, files, and Git in one window

License:        GPL-3.0-or-later
URL:            https://github.com/unisic/tuni
Source0:        %{name}-%{version}.tar.gz

BuildRequires:  cargo
BuildRequires:  rust >= 1.95
BuildRequires:  gcc
BuildRequires:  make
BuildRequires:  pkgconfig(gtk4)
BuildRequires:  pkgconfig(libadwaita-1)
BuildRequires:  pkgconfig(gtksourceview-5)
# OpenCode keeps its sessions in SQLite, and the Info page reads them.
BuildRequires:  pkgconfig(sqlite3)
BuildRequires:  desktop-file-utils
BuildRequires:  libappstream-glib

# Everything the panels shell out to. curl fetches the plan bars the Info
# page shows next to a running Claude Code, and the host list is read with the
# same ssh that opens a connection.
Requires:       git-core
Requires:       /usr/bin/curl
Requires:       openssh-clients
Requires:       hicolor-icon-theme
# Panes say TERM=xterm-ghostty, which is this package's terminfo. Recommends
# rather than Requires: without it tuni falls back to xterm-256color, so a
# minimal install still works.
Recommends:     ghostty-terminfo

%description
Tuni is a terminal workspace: a window of projects, each with tabs, each tab a
layout of panes holding a terminal, a file, or a diff. The terminal emulation is
Ghostty's own through libghostty-vt; the file tree, the Git panel, the editor,
and the diff viewer sit around it in the same window.

%prep
%autosetup

%build
# Fedora's zig is newer than the pinned Ghostty source builds with, so prefer a
# 0.15.2 on PATH and fetch one only when there is none.
if ! zig version 2>/dev/null | grep -q '^%{zig_version}$'; then
    curl -fsSLO https://ziglang.org/download/%{zig_version}/zig-%{_arch}-linux-%{zig_version}.tar.xz
    tar xf zig-%{_arch}-linux-%{zig_version}.tar.xz
    export PATH="$PWD/zig-%{_arch}-linux-%{zig_version}:$PATH"
fi
make build

%install
make install PREFIX=%{_prefix} DESTDIR=%{buildroot}

%check
desktop-file-validate %{buildroot}%{_datadir}/applications/%{app_id}.desktop
appstream-util validate-relax --nonet \
    %{buildroot}%{_datadir}/metainfo/%{app_id}.metainfo.xml

%files
%license LICENSE
%doc README.md
%{_bindir}/%{name}
%{_datadir}/applications/%{app_id}.desktop
%{_datadir}/metainfo/%{app_id}.metainfo.xml
# Globbed rather than listed: the icons come from `make install-data`, which is
# the one list of installed files, and naming them a second time here is a list
# that goes stale silently — a new icon fails the build as unpackaged, which is
# how the Git page's icon broke the 1.3.0 RPM. Nothing else writes into this
# directory in the buildroot, so the glob is exactly tuni's icons.
%{_datadir}/icons/hicolor/*/*/*.svg

%changelog
* Thu Aug 06 2026 Unisic <contact@unisic.app> - 1.5.1-1
- A launch that names no directory opens a window again. Since 1.5.0 it only
  raised the window already open, so a desktop shortcut bound to Ctrl+Alt+T did
  nothing once Tuni was running. A launch that names a folder still opens that
  folder in the window already running.
- An AI Agents page of switches: whether the agents' session logs are read at
  all, which agents count as one, whether Claude Code's plan bars are asked for
  over the network, whether a tab spins while a turn runs, and whether a turn
  that ends out of sight leaves a mark, raises a notification or rings the bell.
- Pi is read like the other three agents. Its title says nothing about a turn
  running, so it gets no spinner of its own; data/pi carries an extension that
  makes it say so, installed with "pi install".

* Wed Aug 05 2026 Unisic <contact@unisic.app> - 1.5.0-1
- Tuni opens on a fresh shell; restoring the last session is a switch, and it
  is off. A second switch restores each pane's working directory, which is read
  from the kernel for the shells that never send OSC 7.
- A tab dragged off the strip moves rather than closes: onto the desktop it
  becomes a window of its own, onto another window's strip it joins that
  window, onto a project row in the sidebar it moves into that project. The row
  swells and lights up under the tab and holds that light for a moment after it
  lands; the tab's menu says "Move to Project" for the same move without the
  drag. A project row dragged off the sidebar takes every tab in it.
- Ctrl+Shift+R reopens the tab just closed, and the four before it, with its
  panes, its name and the output each terminal had printed.
- A tab spins while a coding agent is thinking in it, and the project's row
  spins beside it. A command that ran ten seconds or longer raises a
  notification when it exits in a tab that is out of sight.
- Ctrl+Shift+Page Up and Page Down walk the scrollback one prompt at a time,
  Ctrl+Alt+I types into every pane of the tab at once, and
  Ctrl+Shift+Backspace wipes a half-typed command whole.
- A project's row can carry an icon or an emoji in place of its folder.
- Opening a folder from a file manager reaches the Tuni already running and
  opens it as a project of its own.
- Every preferences row says what it does in one line, and the session
  settings are a page of their own.

* Mon Aug 03 2026 Unisic <contact@unisic.app> - 1.4.1-1
- The install command in the README runs in fish, which has no process
  substitution: it is a pipe into bash rather than bash reading a substituted
  file.
- The build instructions name every package a build needs, and the four more
  that `make check` wants.

* Mon Aug 03 2026 Unisic <contact@unisic.app> - 1.4.0-1
- Tuni checks whether a newer Tuni has been released, once per run, and offers
  an Update button that runs the installer in a tab where sudo can ask for a
  password. Preferences, Terminal, Updates turns the check off.
- scripts/install.sh opens a menu: what is installed, install or update,
  install an older version from the release page, or remove Tuni with or
  without its settings. It installs the package built for the distribution it
  runs on.
- A project can opt out of starting a new tab where the visible shell is, while
  the rest of them go on following it.
- A translucent window repaints whole, so a sliver of text no longer survives on
  chrome with nothing opaque under it.
- A glyph whose ink reaches past its cell is clipped to the grid instead of
  landing on the tab bar.

* Wed Jul 29 2026 Unisic <contact@unisic.app> - 1.3.0-1
- The scrollback keeps the number of lines the setting names, where a
  ten-thousand-line setting used to hold about nine hundred rows.
- An SFTP transfer shows percent and speed and can be cancelled, and a
  cancelled transfer leaves no half file at either end.
- The Info page opens the project in an installed code editor.
- A pane falls back to xterm-256color where xterm-ghostty has no terminfo.
- The window chrome: a commit graph on the Git page, a sticky New Project under
  the last project, solid dialogs under a translucent window, and a pane grip
  that lights only under the pointer.
- The window goes quiet when it is not active: the panel and diff polls skip
  their tick, the scrollbar fade redraws only when it moved, and the live search
  re-runs on the frame clock. The Info page's editor icons are looked up when
  the page opens rather than before the first frame.
- A closed pane lets go of its handlers, its replay text and its textures, and a
  git child whose stdin write failed is reaped and reports what git itself said.

* Tue Jul 28 2026 Unisic <contact@unisic.app> - 1.2.0-1
- Clicks, drags and the wheel behave the way Ghostty's do, and a URL that is
  only text opens with Ctrl+click.
- A copy is confirmed with a toast, including one an application makes over
  OSC 52.
- The editor drives a language server and a debugger, and a selection grows by
  syntax with Alt+Up. The settings choose the panel's pages and the window's
  shortcuts.
- The window blur asks X11 as well as Wayland, and faint text draws halfway to
  the background.
- Escape closes the command palette, and a taken forward port names the
  process that actually holds the address.

* Mon Jul 27 2026 Unisic <contact@unisic.app> - 1.1.2-1
- Packages are compiled for the baseline CPU instead of the machine that built
  them, so tuni starts on processors without AVX-512 rather than dying on an
  illegal instruction.

* Mon Jul 27 2026 Unisic <contact@unisic.app> - 1.1.1-1
- A file dropped on a terminal lands on the prompt as a quoted argument
  instead of being ignored.
- A project dragged in the sidebar is carried as the row itself rather than as
  a picture of a document.

* Mon Jul 27 2026 Unisic <contact@unisic.app> - 1.1.0-1
- SSH: hosts read from the ssh configuration and from a hosts file of tuni's
  own, a new tab that is a host list, one shared connection per host.
- A file browser on a connection: step into directories at the far end, copy
  files both ways, make, rename and delete them there.
- Forwarded ports listed beside a connection, opened and closed while it is up.
- Snippets typed into a pane by name, and the keys in ~/.ssh listed with the
  commands that change them.
- A menu on a right click in the terminal, and a pane that runs something other
  than a login shell.

* Mon Jul 27 2026 Unisic <contact@unisic.app> - 1.0.1-1
- Mouse reporting, focus reporting, and box drawing characters stroked in the
  terminal rather than left to the font.
- Background opacity, blur, and padding; the font list the machine actually has.
- An about dialog, plan usage drawn as bars, and a settings window that carries
  more of the settings.
- A Files panel beside the tab strip rather than under it, taking a typed path
  or a step to the parent directory.
- Fixes: panes that closed without being forgotten, a command palette that kept
  the window alive, a resize that stranded the prompt.

* Sun Jul 26 2026 Unisic <contact@unisic.app> - 1.0.0-1
- First release.
