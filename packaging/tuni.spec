# RPM spec, for Fedora and everything that follows it.
#
# The build fetches: cargo dependencies, and the pinned Ghostty source that
# libghostty-vt is compiled from. That rules out Koji, which builds offline —
# this is for COPR with network enabled, or for a local `rpmbuild`. Set
# GHOSTTY_SOURCE_DIR to a checkout of the pinned commit and CARGO_HOME to a
# populated registry to build it without either.
#
#   make -C .. dist   # or: git archive --prefix=tuni-1.1.0/ -o tuni-1.1.0.tar.gz HEAD
#   rpmbuild -ba packaging/tuni.spec

%global app_id dev.unisic.Tuni
# Zig 0.15.2, which is what the pinned Ghostty source builds with. Fedora's own
# zig package moves faster than that, so the build uses whatever is on PATH and
# falls back to fetching this one.
%global zig_version 0.15.2

Name:           tuni
Version:        1.1.0
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
%{_datadir}/icons/hicolor/scalable/apps/%{app_id}.svg
%{_datadir}/icons/hicolor/symbolic/apps/%{app_id}-symbolic.svg

%changelog
* Mon Jul 27 2026 Unisic <hello@unisic.dev> - 1.1.0-1
- SSH: hosts read from the ssh configuration and from a hosts file of tuni's
  own, a new tab that is a host list, one shared connection per host.
- A file browser on a connection: step into directories at the far end, copy
  files both ways, make, rename and delete them there.
- Forwarded ports listed beside a connection, opened and closed while it is up.
- Snippets typed into a pane by name, and the keys in ~/.ssh listed with the
  commands that change them.
- A menu on a right click in the terminal, and a pane that runs something other
  than a login shell.

* Mon Jul 27 2026 Unisic <hello@unisic.dev> - 1.0.1-1
- Mouse reporting, focus reporting, and box drawing characters stroked in the
  terminal rather than left to the font.
- Background opacity, blur, and padding; the font list the machine actually has.
- An about dialog, plan usage drawn as bars, and a settings window that carries
  more of the settings.
- A Files panel beside the tab strip rather than under it, taking a typed path
  or a step to the parent directory.
- Fixes: panes that closed without being forgotten, a command palette that kept
  the window alive, a resize that stranded the prompt.

* Sun Jul 26 2026 Unisic <hello@unisic.dev> - 1.0.0-1
- First release.
