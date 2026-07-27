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
# decided by the Ghostty source it is compiled from, not by taste:
#
#   0.15.2  the Ghostty commit libghostty-rs pins. The default, and what CI,
#           the RPM spec and the Flatpak manifest all build with.
#   0.16.0  what Fedora 44 and other current distributions ship. Ghostty's own
#           main requires it and the pinned commit rejects it, so 0.16 also
#           means a newer Ghostty, passed in through GHOSTTY_SOURCE_DIR.
#
# `make zig` fetches the default toolchain; `make build-next` builds the whole
# thing the 0.16 way. Keep the pin in step with .github/workflows/ci.yml,
# packaging/tuni.spec and packaging/dev.unisic.Tuni.yml.
ZIG_VERSION ?= 0.15.2
ZIG_NEXT_VERSION := 0.16.0
ZIG_PREFIX ?= $(HOME)/.local
ZIG_ARCH := $(shell uname -m)
ZIG_DIR = zig-$(ZIG_ARCH)-linux-$(ZIG_VERSION)
ZIG_TAR = $(ZIG_DIR).tar.xz
ZIG_SHA256_0.15.2_x86_64 := 02aa270f183da276e5b5920b1dac44a63f1a49e55050ebde3aecc9eb82f93239
ZIG_SHA256_0.15.2_aarch64 := 958ed7d1e00d0ea76590d27666efbf7a932281b3d7ba0c6b01b0ff26498f667f
ZIG_SHA256_0.16.0_x86_64 := 70e49664a74374b48b51e6f3fdfbf437f6395d42509050588bd49abe52ba3d00
ZIG_SHA256_0.16.0_aarch64 := ea4b09bfb22ec6f6c6ceac57ab63efb6b46e17ab08d21f69f3a48b38e1534f17
ZIG_SHA256 = $(ZIG_SHA256_$(ZIG_VERSION)_$(ZIG_ARCH))
# Whether the fetched toolchain also becomes the `zig` on PATH. The 0.16 one
# does not: it is for this build and not for everything else on the machine.
ZIG_LINK ?= yes

# Which CPU the Zig half is compiled for. Left alone, zig detects the machine
# doing the building, which is right for a build that stays there and wrong for
# a package: the 1.1.1 binaries were compiled on a runner with AVX-512 and died
# on SIGILL on the first machine without it. baseline is what a package wants;
# ZIG_CPU=native is for a build that never leaves this machine.
ZIG_CPU ?= baseline

# Ghostty's main branch moves and the C API the bindings are generated from
# moves with it, so the 0.16 route is a commit that was built and tested
# against this tree, not a branch name.
GHOSTTY_NEXT_COMMIT := 2de5e7d38e1354759211722a8687c0815d2cf02c
GHOSTTY_NEXT_DIR ?= $(ZIG_PREFIX)/opt/ghostty-$(GHOSTTY_NEXT_COMMIT)

.PHONY: all build build-next test-next install install-data uninstall check \
	dist clean zig zig-next ghostty-next

all: build

build:
	@zig version 2>/dev/null | grep -qx '$(ZIG_VERSION)' || \
		echo "warning: zig $(ZIG_VERSION) is not on PATH; \`make zig\` installs it, \`make build-next\` builds with $(ZIG_NEXT_VERSION) instead"
	@real=$$(command -v zig) || { echo "zig is not on PATH" >&2; exit 1; }; \
	TUNI_REAL_ZIG="$$real" TUNI_ZIG_CPU='$(ZIG_CPU)' \
	PATH="$(CURDIR)/scripts/zig-baseline:$$PATH" \
	$(CARGO) build --release --locked

# The 0.16 route: that toolchain, the Ghostty commit that accepts it, and a
# cargo that sees both. Nothing here changes what a plain `make build` uses.
build-next: NEXT_ARGS := build --release --locked
test-next: NEXT_ARGS := test --locked
build-next test-next: zig-next ghostty-next
	@dir="$(ZIG_PREFIX)/opt/zig-$(ZIG_ARCH)-linux-$(ZIG_NEXT_VERSION)"; \
	if [ -x "$$dir/zig" ]; then PATH="$$dir:$$PATH"; export PATH; fi; \
	zig version | grep -qx '$(ZIG_NEXT_VERSION)' || { \
		echo "zig $(ZIG_NEXT_VERSION) is not on PATH" >&2; exit 1; \
	}; \
	GHOSTTY_SOURCE_DIR="$(GHOSTTY_NEXT_DIR)" $(CARGO) $(NEXT_ARGS)

# Downloads into $(ZIG_PREFIX)/opt and links the binary into $(ZIG_PREFIX)/bin,
# so nothing outside the home directory is touched and the distribution's own
# zig package, if there is one, stays where it is.
zig:
	@if zig version 2>/dev/null | grep -qx '$(ZIG_VERSION)'; then \
		echo "zig $(ZIG_VERSION) is already on PATH"; exit 0; \
	fi; \
	if [ -x "$(ZIG_PREFIX)/opt/$(ZIG_DIR)/zig" ] && [ '$(ZIG_LINK)' != yes ]; then \
		echo "zig $(ZIG_VERSION) is already in $(ZIG_PREFIX)/opt/$(ZIG_DIR)"; exit 0; \
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
	if [ '$(ZIG_LINK)' = yes ]; then \
		ln -sfn "$(ZIG_PREFIX)/opt/$(ZIG_DIR)/zig" "$(ZIG_PREFIX)/bin/zig" && \
		{ command -v zig >/dev/null || \
			echo "warning: $(ZIG_PREFIX)/bin is not on PATH"; }; \
	fi

zig-next:
	@$(MAKE) --no-print-directory zig ZIG_VERSION=$(ZIG_NEXT_VERSION) ZIG_LINK=no

# Only the parts of Ghostty that libghostty-vt needs get compiled, but the
# checkout is the whole repository at that commit — fetched shallow, since the
# history is not what is wanted here.
ghostty-next:
	@if [ "$$(git -C '$(GHOSTTY_NEXT_DIR)' rev-parse HEAD 2>/dev/null)" = '$(GHOSTTY_NEXT_COMMIT)' ]; then \
		echo "ghostty $(GHOSTTY_NEXT_COMMIT) is already in $(GHOSTTY_NEXT_DIR)"; exit 0; \
	fi; \
	rm -rf '$(GHOSTTY_NEXT_DIR)' && mkdir -p '$(GHOSTTY_NEXT_DIR)' && \
	git -C '$(GHOSTTY_NEXT_DIR)' init -q && \
	git -C '$(GHOSTTY_NEXT_DIR)' remote add origin \
		https://github.com/ghostty-org/ghostty.git && \
	git -C '$(GHOSTTY_NEXT_DIR)' fetch -q --depth 1 origin '$(GHOSTTY_NEXT_COMMIT)' && \
	git -C '$(GHOSTTY_NEXT_DIR)' checkout -q FETCH_HEAD && \
	echo "ghostty $(GHOSTTY_NEXT_COMMIT) checked out in $(GHOSTTY_NEXT_DIR)"

# Themes are baked into the binary at build time, so an installed Tuni needs
# nothing beside the executable but its desktop integration.
install: build install-data
	install -Dm755 $(CARGO_TARGET_DIR)/release/tuni $(bindir)/tuni

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

uninstall:
	rm -f $(bindir)/tuni
	rm -f $(datadir)/applications/$(APP_ID).desktop
	rm -f $(datadir)/metainfo/$(APP_ID).metainfo.xml
	rm -f $(icondir)/scalable/apps/$(APP_ID).svg
	rm -f $(icondir)/symbolic/apps/$(APP_ID)-symbolic.svg

check:
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
