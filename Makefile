# Building is cargo's job; this is only the part cargo has no opinion about —
# where the binary, the icons, the desktop entry and the AppStream data go.
#
# Every packaging target in packaging/ calls into here, so there is one list of
# installed files rather than one per package format.

PREFIX ?= /usr/local
DESTDIR ?=
CARGO ?= cargo
APP_ID := dev.unisic.Tuni
VERSION := $(shell sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)

bindir = $(DESTDIR)$(PREFIX)/bin
datadir = $(DESTDIR)$(PREFIX)/share
icondir = $(datadir)/icons/hicolor

# Where cargo actually put the binary. Usually ./target, but CARGO_TARGET_DIR
# or a build.target-dir in .cargo/config.toml moves it — a checkout on a
# filesystem without symlinks has to move it, since the libghostty-vt build
# links libghostty-vt.so.0 inside the target directory. Ask cargo rather than
# assume.
CARGO_TARGET_DIR := $(shell $(CARGO) metadata --format-version 1 --no-deps 2>/dev/null \
	| sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')
CARGO_TARGET_DIR := $(or $(CARGO_TARGET_DIR),target)

# libghostty-vt is Zig source compiled during the build, so which Zig works is
# decided by the Ghostty source it is compiled from, not by taste. Ghostty 1.4
# declares 0.17.0 its minimum, and Fedora 44 still ships 0.16, so `make zig`
# fetches it. Keep the pin in step with .github/workflows/ci.yml,
# packaging/tuni.spec and packaging/dev.unisic.Tuni.yml.
ZIG_VERSION ?= 0.17.0
ZIG_PREFIX ?= $(HOME)/.local
ZIG_ARCH := $(shell uname -m)
ZIG_DIR = zig-$(ZIG_ARCH)-linux-$(ZIG_VERSION)
ZIG_TAR = $(ZIG_DIR).tar.xz
ZIG_SHA256_0.17.0_x86_64 := 1cbe9df9f27e6b78d14ccbca43b6703a404ef79ef1c463de901d7f088d4e2026
ZIG_SHA256_0.17.0_aarch64 := 9e8d11661d4ae3bd57702a3832781e23ad151dde5798e16a5ccd503f65234ff8
ZIG_SHA256 = $(ZIG_SHA256_$(ZIG_VERSION)_$(ZIG_ARCH))

# Which CPU the Zig half is compiled for. Left alone, zig detects the machine
# doing the building, which is right for a build that stays there and wrong for
# a package: the 1.1.1 binaries were compiled on a runner with AVX-512 and died
# on SIGILL on the first machine without it. The bindings pass it to zig from
# LIBGHOSTTY_VT_SYS_CPU and default to baseline, which is what a package wants;
# ZIG_CPU=native is for a build that never leaves this machine.
ZIG_CPU ?= baseline

# Which Ghostty is compiled is libghostty-rs's choice unless GHOSTTY_SOURCE_DIR
# names another, and the commit it pins, 521cf2c5, does not have the fix for
# GHSA-w9q4-5jvv-mcv4: clearing the screen into a small scrollback could free
# the page the cursor stood on while the cursor kept its style and hyperlink
# ids. So every build through here hands it the 1.4.0 release instead, and
# crates/tuni-vt/tests/feed.rs carries the input that aborted the Ghostty this
# tree compiled before. The checksum is of the tarball whose minisign signature
# checks out against the key in Ghostty's PACKAGING.md. Once the bindings pin
# 1.4.0 or later, this and the `ghostty` target go. A build that sets
# GHOSTTY_SOURCE_DIR itself, offline or not, keeps its own and nothing is
# fetched.
GHOSTTY_VERSION := 1.4.0
GHOSTTY_SHA256 := cb188ac6c3a8278d5fd7bfef15d8c017104c87f638b5560eb38024a71ae0f005
export GHOSTTY_SOURCE_DIR ?= $(CARGO_TARGET_DIR)/ghostty-$(GHOSTTY_VERSION)

.PHONY: all build install install-data uninstall check dist clean zig \
	ghostty ghostty-dir dev

all: build

build: ghostty
	@zig version 2>/dev/null | grep -qx '$(ZIG_VERSION)' || \
		echo "warning: zig $(ZIG_VERSION) is not on PATH; \`make zig\` installs it"
	@LIBGHOSTTY_VT_SYS_CPU='$(ZIG_CPU)' $(CARGO) build --release --locked

# Downloads into $(ZIG_PREFIX)/opt and links the binary into $(ZIG_PREFIX)/bin,
# so nothing outside the home directory is touched and the distribution's own
# zig package, if there is one, stays where it is.
zig:
	@if zig version 2>/dev/null | grep -qx '$(ZIG_VERSION)'; then \
		echo "zig $(ZIG_VERSION) is already on PATH"; exit 0; \
	fi; \
	if [ -z "$(ZIG_SHA256)" ]; then \
		echo "no pinned Zig $(ZIG_VERSION) tarball for $(ZIG_ARCH)" >&2; exit 1; \
	fi; \
	tmp=$$(mktemp -d) && trap 'rm -rf "$$tmp"' EXIT && \
	curl -fsSL -o "$$tmp/$(ZIG_TAR)" \
		"https://ziglang.org/download/$(ZIG_VERSION)/$(ZIG_TAR)" && \
	echo "$(ZIG_SHA256)  $$tmp/$(ZIG_TAR)" | sha256sum -c - && \
	mkdir -p "$(ZIG_PREFIX)/opt" "$(ZIG_PREFIX)/bin" && \
	rm -rf "$(ZIG_PREFIX)/opt/$(ZIG_DIR)" && \
	tar xf "$$tmp/$(ZIG_TAR)" -C "$(ZIG_PREFIX)/opt" && \
	echo "zig $(ZIG_VERSION) installed in $(ZIG_PREFIX)/opt/$(ZIG_DIR)" && \
	ln -sfn "$(ZIG_PREFIX)/opt/$(ZIG_DIR)/zig" "$(ZIG_PREFIX)/bin/zig" && \
	{ command -v zig >/dev/null || \
		echo "warning: $(ZIG_PREFIX)/bin is not on PATH"; }

# Unpacked inside the target directory rather than the home directory, so a
# package build needs nothing writable outside its own tree. It is unpacked
# beside its destination and renamed onto it, so an interrupted fetch leaves no
# half a tree for the next build to trust. A directory that is there without a
# build.zig was not put there by this target and is not removed by it either.
ghostty:
	@if [ -f '$(GHOSTTY_SOURCE_DIR)/build.zig' ]; then exit 0; fi; \
	if [ -e '$(GHOSTTY_SOURCE_DIR)' ]; then \
		echo "$(GHOSTTY_SOURCE_DIR) is not a Ghostty source tree" >&2; exit 1; \
	fi; \
	mkdir -p '$(dir $(GHOSTTY_SOURCE_DIR))' && \
	tmp=$$(mktemp -d '$(GHOSTTY_SOURCE_DIR).XXXXXX') && trap 'rm -rf "$$tmp"' EXIT && \
	curl -fsSL -o "$$tmp/ghostty.tar.gz" \
		'https://release.files.ghostty.org/$(GHOSTTY_VERSION)/ghostty-$(GHOSTTY_VERSION).tar.gz' && \
	echo "$(GHOSTTY_SHA256)  $$tmp/ghostty.tar.gz" | sha256sum -c - && \
	mkdir "$$tmp/src" && \
	tar xzf "$$tmp/ghostty.tar.gz" -C "$$tmp/src" --strip-components=1 && \
	mv -T "$$tmp/src" '$(GHOSTTY_SOURCE_DIR)' && \
	echo "ghostty $(GHOSTTY_VERSION) unpacked in $(GHOSTTY_SOURCE_DIR)"

# For a cargo run that does not go through this Makefile, CI's clippy and test
# steps among them, to be pointed at the same source.
ghostty-dir:
	@echo '$(GHOSTTY_SOURCE_DIR)'

# Themes are baked into the binary at build time, so an installed Tuni needs
# nothing beside the executable but its desktop integration.
# The build being worked on, beside the installed Tuni rather than inside it.
# Under the installed one's ID it would be handed to the Tuni already running,
# which opens a window of the old version; under its own it is an application
# of its own, with its own taskbar entry. The saved session is left alone, so
# closing it cannot overwrite the real one. Not single either: a second
# `make dev` with the first still open would otherwise hand its window to the
# first, which is the build from before.
dev: build
	TUNI_APP_ID=$(APP_ID).Devel TUNI_SESSION=off TUNI_SINGLE_INSTANCE=0 $(CARGO_TARGET_DIR)/release/tuni

install: build install-data
	install -Dm755 $(CARGO_TARGET_DIR)/release/tuni $(bindir)/tuni
	install -Dm755 $(CARGO_TARGET_DIR)/release/tuni-gtk $(bindir)/tuni-gtk

# The window names its icon rather than carrying one, so a Tuni run straight
# out of the build directory shows the desktop's fallback icon until the entry
# and the icon are somewhere the compositor looks:
#
#   make install-data PREFIX=$$HOME/.local
install-data:
	install -Dm644 data/$(APP_ID).desktop $(datadir)/applications/$(APP_ID).desktop
	install -Dm644 data/$(APP_ID).metainfo.xml $(datadir)/metainfo/$(APP_ID).metainfo.xml
	install -Dm644 data/icons/hicolor/scalable/apps/$(APP_ID).svg \
		$(icondir)/scalable/apps/$(APP_ID).svg
	install -Dm644 data/icons/hicolor/symbolic/apps/$(APP_ID)-symbolic.svg \
		$(icondir)/symbolic/apps/$(APP_ID)-symbolic.svg
	# The whole directory rather than a file per line: the icons Adwaita does
	# not have are added a few at a time, and a list here that has to be
	# remembered is a list that goes stale silently.
	for icon in data/icons/hicolor/scalable/actions/*.svg; do \
		install -Dm644 $$icon $(icondir)/scalable/actions/$$(basename $$icon); \
	done

uninstall:
	rm -f $(bindir)/tuni $(bindir)/tuni-gtk
	rm -f $(datadir)/applications/$(APP_ID).desktop
	rm -f $(datadir)/metainfo/$(APP_ID).metainfo.xml
	rm -f $(icondir)/scalable/apps/$(APP_ID).svg
	rm -f $(icondir)/symbolic/apps/$(APP_ID)-symbolic.svg
	for icon in data/icons/hicolor/scalable/actions/*.svg; do \
		rm -f $(icondir)/scalable/actions/$$(basename $$icon); \
	done

check: ghostty
	$(CARGO) fmt --check
	$(CARGO) clippy --all-targets -- -D warnings
	$(CARGO) test
	desktop-file-validate data/$(APP_ID).desktop
	appstreamcli validate --no-net data/$(APP_ID).metainfo.xml

# The tarball rpmbuild expects, straight out of git so nothing untracked
# travels with it.
dist:
	git archive --prefix=tuni-$(VERSION)/ -o tuni-$(VERSION).tar.gz HEAD

clean:
	$(CARGO) clean
	rm -f tuni-$(VERSION).tar.gz
