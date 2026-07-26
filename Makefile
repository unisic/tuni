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

# libghostty-vt is Zig source compiled during the build, and the pinned Ghostty
# commit builds with exactly this Zig: the standard library changed under it in
# 0.16, which is already what Fedora ships. Distributions move faster than the
# pin, so `make zig` puts the official tarball in a home prefix instead of
# arguing with the packaged one. Keep in step with .github/workflows/ci.yml,
# packaging/tuni.spec and packaging/dev.unisic.Tuni.yml, which pin it too.
ZIG_VERSION := 0.15.2
ZIG_PREFIX ?= $(HOME)/.local
ZIG_ARCH := $(shell uname -m)
ZIG_DIR := zig-$(ZIG_ARCH)-linux-$(ZIG_VERSION)
ZIG_TAR := $(ZIG_DIR).tar.xz
ZIG_SHA256_x86_64 := 02aa270f183da276e5b5920b1dac44a63f1a49e55050ebde3aecc9eb82f93239
ZIG_SHA256_aarch64 := 958ed7d1e00d0ea76590d27666efbf7a932281b3d7ba0c6b01b0ff26498f667f
ZIG_SHA256 := $(ZIG_SHA256_$(ZIG_ARCH))

.PHONY: all build install install-data uninstall check dist clean zig

all: build

build:
	@zig version 2>/dev/null | grep -qx '$(ZIG_VERSION)' || \
		echo "warning: zig $(ZIG_VERSION) is not on PATH; \`make zig\` installs it"
	$(CARGO) build --release --locked

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
	ln -sfn "$(ZIG_PREFIX)/opt/$(ZIG_DIR)/zig" "$(ZIG_PREFIX)/bin/zig" && \
	echo "zig $(ZIG_VERSION) installed in $(ZIG_PREFIX)/opt/$(ZIG_DIR)"; \
	command -v zig >/dev/null || \
		echo "warning: $(ZIG_PREFIX)/bin is not on PATH"

# Themes are baked into the binary at build time, so an installed Tuni needs
# nothing beside the executable but its desktop integration.
install: build install-data
	install -Dm755 target/release/tuni $(bindir)/tuni

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
