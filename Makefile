.PHONY: help build check test fmt lint clean install
PREFIX ?= $(HOME)/.local/share
BINDIR ?= $(HOME)/.local/bin

help:
	@echo 'build: release binaries; install: build, copy binaries and teams, link into BINDIR'
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

install: build
	mkdir -p "$(PREFIX)/brg/bin" "$(BINDIR)"
	for name in brg borgchart brg-seat; do \
		cp "target/release/$$name" "$(PREFIX)/brg/bin/$$name.tmp" && mv -f "$(PREFIX)/brg/bin/$$name.tmp" "$(PREFIX)/brg/bin/$$name" || exit 1; \
	done
	rm -rf "$(PREFIX)/brg/teams.tmp"
	cp -R teams "$(PREFIX)/brg/teams.tmp"
	rm -rf "$(PREFIX)/brg/teams"
	mv "$(PREFIX)/brg/teams.tmp" "$(PREFIX)/brg/teams"
	git rev-parse HEAD > "$(PREFIX)/brg/REVISION"
	for name in brg borgchart brg-seat; do ln -sfn "$(PREFIX)/brg/bin/$$name" "$(BINDIR)/$$name" || exit 1; done
	@printf 'teams: [%s/brg/teams]\n' "$(PREFIX)"
