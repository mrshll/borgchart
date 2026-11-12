.PHONY: help build check test fmt lint clean install upgrade
PREFIX ?= $(HOME)/.local/share
BINDIR ?= $(HOME)/.local/bin

help:
	@echo 'build: release binaries; install: build, copy binaries, link into BINDIR'
	@echo 'upgrade: build, collect handovers, stop trees, install, start fresh (TIMEOUT=1800; FORCE=1 skips handover and working check; DRY_RUN=1 previews)'
	@echo 'check: quick checks; test: full checks; lint: clippy'
	@echo 'fmt: format Rust; clean: remove build artifacts'

build:
	if [ "$$(uname -s)" = Linux ] && [ -x tools/cargo-slot ]; then tools/cargo-slot cargo build --release --workspace --bins --locked; else cargo build --release --workspace --bins --locked; fi

check:
	.githooks/checks fmt clippy fast

test:
	.githooks/checks

fmt:
	cargo fmt --all

lint:
	.githooks/checks clippy

clean:
	cargo clean

upgrade:
	@MAKE="$(MAKE_COMMAND)" BRG="$(or $(BRG),brg)" TIMEOUT="$(TIMEOUT)" FORCE="$(FORCE)" DRY_RUN="$(DRY_RUN)" "$(abspath $(dir $(lastword $(MAKEFILE_LIST))))/tools/upgrade"

install: build
	mkdir -p "$(PREFIX)/brg/bin" "$(BINDIR)"
	for name in brg brg-seat; do \
		cp "target/release/$$name" "$(PREFIX)/brg/bin/$$name.tmp" && mv -f "$(PREFIX)/brg/bin/$$name.tmp" "$(PREFIX)/brg/bin/$$name" || exit 1; \
	done
	git rev-parse HEAD > "$(PREFIX)/brg/REVISION"
	for name in brg brg-seat; do ln -sfn "$(PREFIX)/brg/bin/$$name" "$(BINDIR)/$$name" || exit 1; done
