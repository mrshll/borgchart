#!/bin/sh
# Install a published release; BRG_VERSION=v0.1.0 pins a release.
# BRG_RELEASE_URL overrides its download directory (also accepts file://).
set -eu

main() {
    case "$(uname -s)" in
        Linux) os=unknown-linux-gnu ;;
        Darwin) os=apple-darwin ;;
        *) echo "This operating system is unsupported; build brg from source." >&2; exit 1 ;;
    esac
    case "$(uname -m)" in
        x86_64|amd64) arch=x86_64 ;;
        aarch64|arm64) arch=aarch64 ;;
        *) echo "This architecture is unsupported; build brg from source." >&2; exit 1 ;;
    esac
    command -v curl >/dev/null 2>&1 || {
        echo "Install curl, then run this installer again." >&2; exit 1;
    }
    version=${BRG_VERSION:-}
    if [ -z "$version" ]; then
        latest=$(curl -fsSL -o /dev/null -w '%{url_effective}' https://github.com/mrshll/borgchart/releases/latest)
        version=${latest##*/}
    fi
    if ! printf '%s\n' "$version" | grep -Eq '^v[0-9]+\.[0-9]+\.[0-9]+$'; then
        echo "No stable release was found; set BRG_VERSION to a published vX.Y.Z tag." >&2
        exit 1
    fi
    archive="brg-${version#v}-$arch-$os.tar.gz"
    base=${BRG_RELEASE_URL:-https://github.com/mrshll/borgchart/releases/download/$version}
    prefix=${PREFIX:-$HOME/.local/share}
    bindir=${BINDIR:-$HOME/.local/bin}
    config_dir=$HOME/.config/borgchart
    case "$prefix:$bindir:$config_dir" in
        /*:/*:/*) ;;
        *) echo "Use absolute paths for PREFIX, BINDIR and the config directory." >&2; exit 1 ;;
    esac
    download=$(mktemp -d)
    stage=
    trap 'rm -rf "$download"; [ -z "$stage" ] || rm -rf "$stage"' 0
    trap 'exit 1' HUP INT TERM
    curl -fsSL "$base/$archive" -o "$download/$archive"
    curl -fsSL "$base/SHA256SUMS" -o "$download/SHA256SUMS"
    expected=$(awk -v name="$archive" '$2 == name {print $1}' "$download/SHA256SUMS")
    if command -v sha256sum >/dev/null 2>&1; then
        actual=$(sha256sum "$download/$archive")
    elif command -v shasum >/dev/null 2>&1; then
        actual=$(shasum -a 256 "$download/$archive")
    else
        echo "Install sha256sum or shasum, then run this installer again." >&2
        exit 1
    fi
    actual=${actual%% *}
    if [ -z "$expected" ] || [ "$expected" != "$actual" ]; then
        echo "The release checksum does not match; download it again." >&2
        exit 1
    fi
    tar -xzf "$download/$archive" -C "$download"
    for name in brg brg-seat brg-slack brg-timer brg-github; do
        [ -x "$download/bin/$name" ] || {
            echo "The release is missing $name; download a complete release." >&2; exit 1;
        }
    done
    [ -d "$download/templates" ] && [ -f "$download/LICENSE" ] || {
        echo "The release is missing templates or LICENSE; download a complete release." >&2
        exit 1
    }
    mkdir -p "$prefix/brg/bin" "$bindir"
    stage=$(mktemp -d "$prefix/brg/.install.XXXXXX")
    cp -R "$download/templates" "$stage/templates"
    cp "$download/LICENSE" "$stage/LICENSE"
    for name in brg brg-seat brg-slack brg-timer brg-github; do
        cp "$download/bin/$name" "$stage/$name"
        chmod 755 "$stage/$name"
        mv -f "$stage/$name" "$prefix/brg/bin/$name"
        ln -sfn "$prefix/brg/bin/$name" "$bindir/$name"
    done
    # Stage the complete catalog before replacing the previous one.
    if [ -e "$prefix/brg/templates" ] || [ -L "$prefix/brg/templates" ]; then
        mv "$prefix/brg/templates" "$stage/previous-templates"
    fi
    mv "$stage/templates" "$prefix/brg/templates"
    mv -f "$stage/LICENSE" "$prefix/brg/LICENSE"
    templates="$prefix/brg/templates"
    quoted=$(printf '%s' "$templates" | sed "s/'/''/g")
    mkdir -p "$config_dir"
    config="$config_dir/config.yaml"
    if [ ! -e "$config" ] && [ ! -L "$config" ]; then
        (umask 077; printf "schema: 1\nauto_trust: true\ntemplates: ['%s']\n" "$quoted" > "$stage/config.yaml")
        mv "$stage/config.yaml" "$config"
    elif ! grep -Fq "$templates" "$config" && ! grep -Fq "$quoted" "$config"; then
        printf "Add this entry under templates in %s:\n  - '%s'\n" "$config" "$quoted"
    fi
    for tool in git claude codex; do
        if ! command -v "$tool" >/dev/null 2>&1; then
            printf 'Missing %s; install it before starting a team.\n' "$tool"
        fi
    done
    if ! command -v tmux >/dev/null 2>&1; then
        echo "Missing tmux; install tmux 3.2 or newer."
    elif ! tmux -V | awk '{split($2, v, "."); exit !(v[1] > 3 || (v[1] == 3 && v[2]+0 >= 2))}'; then
        echo "Upgrade tmux to 3.2 or newer."
    fi
    case ":${PATH:-}:" in
        *:"$bindir":*) ;;
        *) printf 'Add %s to PATH before starting a team.\n' "$bindir" ;;
    esac
    printf 'Installed brg %s. Log in to claude and codex, then run:\n' "$version"
    printf 'cd path/to/your/repository\nbrg create engineering\n'
}

main "$@"
