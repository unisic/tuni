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

.PHONY: all build install uninstall check dist clean

all: build

build:
	$(CARGO) build --release --locked

# Themes are baked into the binary at build time, so an installed Tuni needs
# nothing beside the executable but its desktop integration.
install: build
	install -Dm755 target/release/tuni $(bindir)/tuni
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
