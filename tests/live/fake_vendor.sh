#!/bin/sh
# The live tier's dry-run stand-in for `claude` and `codex` (installed under
# those names). It draws just enough of each vendor's screen for brg's state
# rules, and acts out one scenario per member, named in
# $HOME/fake/scenario-<member> (default `ok`):
#   ok           start normally (and, for `waiter`, wait in the background)
#   nocolour     no RGB colour: a 256-colour palette instead
#   wrongenv     re-exec with TERM=dumb and no COLORTERM
#   approval     a tool-approval screen
#   never-armed  `waiter` never starts its wait
#   foreground   `waiter` waits while showing a running turn
#   stale        `waiter` replies with $HOME/fake/stale-nonce, reversed
#   write-config append a line to $CODEX_HOME/config.toml at start
#   write-at-exit append to $CODEX_HOME/config.toml when stopped
#   trust-exit   show the trust screen briefly, then exit
#   trust-unknown show the trust screen briefly, then an unknown screen
#   early-turn   `waiter` starts its turn on the message while its wait
#                still runs, and replies after the wait ends
#   same-poll    `waiter`, already working, replies just before its wait
#                ends (both within one poll interval)
# early-turn and same-poll end a decoy wait (one with a wrong identity,
# which brg keeps running for its 60 s minimum) themselves, so they choose
# when it exits.
# $HOME/fake/hang-version-<vendor> makes `--version` hang.
# Folders listed in $HOME/fake/trusted (and below them) are trusted.
set -u
vendor=$(basename "$0")
fake="$HOME/fake"
# Behaviour is keyed on the role: members carry a suffixed ID.
member=${BORGCHART_ROLE:-}
for a in "$@"; do
  if [ "$a" = --version ]; then
    [ -e "$fake/hang-version-$vendor" ] && exec sleep 3600
    echo "fake $vendor 0.0.0"
    exit 0
  fi
done
scenario=ok
[ -n "$member" ] && [ -f "$fake/scenario-$member" ] && scenario=$(cat "$fake/scenario-$member")

if [ "$scenario" = wrongenv ] && [ -z "${FAKE_REEXEC:-}" ]; then
  exec env -u COLORTERM TERM=dumb FAKE_REEXEC=1 /bin/sh "$0" "$@"
fi
if [ "$scenario" = write-at-exit ] && [ -n "${CODEX_HOME:-}" ]; then
  trap 'echo "# written at exit by the fake" >>"$CODEX_HOME/config.toml"; exit 0' HUP TERM INT
fi
if [ "$scenario" = write-config ] && [ -n "${CODEX_HOME:-}" ]; then
  echo "# written by the fake" >>"$CODEX_HOME/config.toml"
fi

colour='\033[38;2;215;119;87m'
[ "$scenario" = nocolour ] && colour='\033[38;5;208m'
rule=────────────────────────────────────────────────────────────
lines=

# Stay running as this process, so its argv and environment stay readable.
forever() { while :; do sleep 5; done; }

say() { lines="$lines$1
"; }

draw() { # draw <mode>: idle | working | trust | codex-trust | approval
  printf '\033[2J\033[H'
  case $1 in
    trust)
      printf '\033]0;\007%s\r\n Accessing workspace:\r\n\r\n %s\r\n\r\n' "$rule" "$PWD"
      printf ' Quick safety check: Is this a project you created or one you trust?\r\n\r\n'
      printf ' ❯ No, exit\r\n   Yes, I trust this folder\r\n\r\n Enter to confirm · Esc to cancel\r\n'
      return ;;
    codex-trust)
      printf '\033]0;\007  Folder access\r\n  %s\r\n' "$PWD"
      printf '  Trust this folder? Your trust decision will be saved.\r\n'
      printf '› 1. Trust and continue\r\n  2. Quit\r\n  enter continue · esc quit\r\n'
      return ;;
  esac
  if [ "$vendor" = claude ]; then
    case $1 in
      working) printf '\033]0;\342\227\220 fake\007' ;;
      *) printf '\033]0;\342\234\263 fake\007' ;;
    esac
    printf "${colour}▝▜██████▀\033[0m  Fake Claude\r\n"
    printf '%s' "$lines" | while IFS= read -r l; do printf '%s\r\n' "$l"; done
    case $1 in
      idle) printf '%s\r\n❯ \r\n%s\r\n  ⏵⏵ accept edits on\r\n' "$rule" "$rule" ;;
      working) printf '✻ Working… (1s)\r\n%s\r\n❯ \r\n%s\r\n  esc to interrupt\r\n' "$rule" "$rule" ;;
      approval)
        printf ' Bash command\r\n This command requires approval\r\n Do you want to proceed?\r\n'
        printf ' ❯ 1. Yes\r\n   2. No\r\n Esc to cancel · Tab to amend\r\n' ;;
    esac
  else
    printf '\033]0;fake\007'
    printf "${colour}>_ Fake Codex\033[0m\r\n"
    printf '%s' "$lines" | while IFS= read -r l; do printf '%s\r\n' "$l"; done
    case $1 in
      idle) printf '› Ask Codex to do anything\r\n\r\n  ? for shortcuts\r\n' ;;
      working) printf '• Working (1s • esc to interrupt)\r\n› Ask Codex to do anything\r\n\r\n  ? for shortcuts\r\n' ;;
      approval) printf '  Would you like to run the following command?\r\n› 1. Yes, proceed (y)\r\n  Press enter to confirm or esc to cancel\r\n' ;;
    esac
  fi
}

trusted() {
  [ -f "$fake/trusted" ] || return 1
  while IFS= read -r dir; do
    case $PWD/ in "$dir"/*) return 0 ;; esac
  done <"$fake/trusted"
  return 1
}

# The trust screen, left unanswered (or, for the controls, not kept).
at_trust() {
  draw "$1"
  case $scenario in
    trust-exit) sleep 1; exit 1 ;;
    trust-unknown) sleep 1; printf '\033[2J\033[H  something else entirely\r\n' ;;
  esac
  forever
}

if ! trusted; then
  [ "$vendor" = claude ] && at_trust trust
  want="projects={\"$PWD\"={trust_level=\"trusted\"}}"
  ok=
  for a in "$@"; do [ "$a" = "$want" ] && ok=1; done
  [ -z "$ok" ] && at_trust codex-trust
fi

if [ "$scenario" = approval ]; then
  draw approval
  forever
fi

nonce_word() { # the nonce from the inbox, read without marking it shown
  "$BORGCHART_BIN" inbox --member "$BORGCHART_MEMBER" --full | grep -o 'nonce [0-9a-z]*' | tail -1 | cut -d' ' -f2
}

reply() { say "● REPLY $(printf '%s' "$1" | rev)"; draw idle; }

if [ "$member" = waiter ]; then
  out=$(mktemp)
  case $scenario in
    never-armed)
      say "● ARMED"
      draw idle
      forever ;;
    foreground)
      draw working
      "$BORGCHART_BIN" inbox wait --background --timeout 600 >"$out" 2>&1 ;;
    early-turn|same-poll)
      BORGCHART_INCARNATION=not-this-one "$BORGCHART_BIN" inbox wait --background >"$out" 2>&1 &
      decoy=$!
      say "● ARMED"
      draw idle
      until [ -n "$(nonce_word)" ]; do sleep 0.1; done
      draw working
      if [ "$scenario" = early-turn ]; then
        sleep 1
        kill $decoy
        sleep 0.5
      else
        reply "$(nonce_word)"
        kill $decoy
      fi
      [ "$scenario" = early-turn ] && reply "$(nonce_word)"
      forever ;;
    *)
      "$BORGCHART_BIN" inbox wait --background --timeout 600 >"$out" 2>&1 &
      say "● ARMED"
      draw idle
      wait $! ;;
  esac
  # The new turn, after the wait ended.
  draw working
  sleep 1
  if [ "$scenario" = stale ]; then
    reply "$(cat "$fake/stale-nonce")"
  else
    reply "$(nonce_word)"
  fi
  forever
fi

say "● READY"
draw idle
forever
