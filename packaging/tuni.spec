# RPM spec, for Fedora and everything that follows it.
#
# The build fetches: cargo dependencies, and the pinned Ghostty source that
# libghostty-vt is compiled from. That rules out Koji, which builds offline —
# this is for COPR with network enabled, or for a local `rpmbuild`. Set
# GHOSTTY_SOURCE_DIR to a checkout of the pinned commit and CARGO_HOME to a
# populated registry to build it without either.
#
#   make -C .. dist   # or: git archive --prefix=tuni-0.1.0/ -o tuni-0.1.0.tar.gz HEAD
#   rpmbuild -ba packaging/tuni.spec

%global app_id dev.unisic.Tuni
# Zig 0.15.2, which is what the pinned Ghostty source builds with. Fedora's own
# zig package moves faster than that, so the build uses whatever is on PATH and
# falls back to fetching this one.
%global zig_version 0.15.2

Name:           tuni
Version:        0.1.0
Release:        1%{?dist}
Summary:        Terminals, projects, files, and Git in one window

License:        GPL-3.0-or-later
URL:            https://github.com/unisic/tuni
Source0:        %{name}-%{version}.tar.gz

BuildRequires:  cargo
BuildRequires:  rust >= 1.90
BuildRequires:  gcc
BuildRequires:  make
BuildRequires:  pkgconfig(gtk4)
BuildRequires:  pkgconfig(libadwaita-1)
BuildRequires:  pkgconfig(gtksourceview-5)
BuildRequires:  desktop-file-utils
BuildRequires:  libappstream-glib

# Everything the panels shell out to.
Requires:       git-core
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
* Sun Jul 26 2026 Unisic <hello@unisic.dev> - 0.1.0-1
- First release.
