#!/bin/bash
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
#   late-trust   draw the startup composer before checking session trust
#   late-trust-no-session ignore session trust after the startup composer
#   late-idle    queue replies finish later; an interrupted large turn
#                loses the stored reply on resume
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
# Queue requests run outside any member, with the recipient's base env.
if [ "$vendor" = codex ] && [ "${1:-}" = queue ]; then
  shift
  [ "${1:-}" = --thread ] || exit 2
  thread=$2
  shift 2
  [ "${1:-}" = --message ] || exit 2
  [ -d "$fake/queue-$thread" ] || exit 1
  [ -e "$fake/drop-queue" ] && exit 0
  input=$(mktemp "$fake/queue-$thread/.incoming.XXXXXX")
  printf '%s' "$2" >"$input"
  mv "$input" "$fake/queue-$thread/$(date +%s%N)-$$"
  echo Queued
  exit 0
fi
scenario=ok
[ -n "$member" ] && [ -f "$fake/scenario-$member" ] && scenario=$(cat "$fake/scenario-$member")

if [ "$scenario" = wrongenv ] && [ -z "${FAKE_REEXEC:-}" ]; then
  exec env -u COLORTERM TERM=dumb FAKE_REEXEC=1 /bin/bash "$0" "$@"
fi
if [ "$scenario" = write-at-exit ] && [ -n "${CODEX_HOME:-}" ]; then
  trap 'printf "\n[projects.\"/tmp/example\"]\ntrust_level = \"trusted\"\n" >>"$CODEX_HOME/config.toml"; exit 0' HUP TERM INT
fi
if [ "$scenario" = write-config ] && [ -n "${CODEX_HOME:-}" ]; then
  printf '\n[projects."/tmp/example"]\ntrust_level = "trusted"\n' >>"$CODEX_HOME/config.toml"
fi
if [ "$scenario" = write-settings ] && [ -n "${CODEX_HOME:-}" ]; then
  printf '\nscreen_reader_detection_done = true\n' >>"$CODEX_HOME/config.toml"
fi

colour='\033[38;2;215;119;87m'
[ "$scenario" = nocolour ] && colour='\033[38;5;208m'
rule=────────────────────────────────────────────────────────────
lines=
buffer=

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
      if [ "${selected:-no}" = yes ]; then
        printf '   No, exit\r\n ❯ Yes, I trust this folder\r\n'
      else
        printf ' ❯ No, exit\r\n   Yes, I trust this folder\r\n'
      fi
      printf '\r\n Enter to confirm · Esc to cancel\r\n'
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
      idle) printf '%s\r\n❯ %s\r\n%s\r\n  -- INSERT -- ⏵⏵ accept edits on\r\n' "$rule" "${buffer//$'\n'/$'\n  '}" "$rule" ;;
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
  if { [ "$member" = typer ] || [ "$member" = waiter ]; } && [ "$1" = idle ]; then
    last=${buffer##*$'\n'}
    printf '\033[3A\033[%sG' "$((${#last} + 3))"
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
  stty raw -echo
  draw "$1"
  if [ "$scenario" = trust-key ]; then
    TMUX_TMPDIR="$BORGCHART_TMUX_TMPDIR" tmux -L "$BORGCHART_TMUX_SOCKET" send-keys -t "=brg-$BORGCHART_TEAM:=$BORGCHART_MEMBER" x
  fi
  case $scenario in
    trust-exit) sleep 1; exit 1 ;;
    trust-unknown) sleep 1; printf '\033[2J\033[H  something else entirely\r\n' ;;
  esac
  [ "$1" = trust ] || forever
  selected=no
  dropped=
  while IFS= read -r -n 1 -d '' key; do
    case $key in
      F|'~')
        if [ "$scenario" = trust-drops-key ] && [ -z "$dropped" ]; then dropped=1; continue; fi
        selected=yes; draw trust ;;
      $'\r'|$'\n') [ "$selected" = yes ] && return ;;
    esac
  done
}

if [[ "$scenario" = late-trust* ]]; then draw idle; sleep 0.3; fi
if [ "$member" != queue-locked ] && ! trusted; then
  if [ "$vendor" = claude ]; then at_trust trust; else
  want="projects={\"$PWD\"={trust_level=\"trusted\"}}"
  ok=
  for a in "$@"; do [ "$a" = "$want" ] && ok=1; done
  git rev-parse --is-inside-work-tree >/dev/null 2>&1 || ok=1
  [ "$scenario" = late-trust-no-session ] && ok=
  [ -z "$ok" ] && at_trust codex-trust
  fi
fi

if [ "$scenario" = approval ]; then
  draw approval
  forever
fi

# The queue case's fakes exercise the real session/gate and plugin handlers.
# Only the vendor turn itself is acted out; no typed input is read here.
case $member in queue-*)
  state="$BORGCHART_STATE_HOME/$BORGCHART_TEAM"
  session_file="$fake/session-$BORGCHART_MEMBER"
  if [ ! -f "$session_file" ]; then
    n=$(printf '%s' "$BORGCHART_MEMBER" | cksum | cut -d' ' -f1)
    printf '00000000-0000-4000-8000-%012d\n' "$n" >"$session_file"
  fi
  thread=$(cat "$session_file")
  inbox="$fake/queue-$thread"
  mkdir -p "$inbox"
  event=session
  post=
  for a in "$@"; do
    case $a in *PostToolUse=*) event=start; post=1 ;; esac
  done
  printf '{"session_id":"%s"}' "$thread" | "$BORGCHART_BIN" _gate "$event" "$state" "$BORGCHART_MEMBER"
  draw working
  sleep 1
  for a in "$@"; do
    case $a in *'brg restarted you'*'msg list --full'*)
      [ -e "$fake/unfinished-$BORGCHART_MEMBER" ] && continue
      for word in $("$BORGCHART_BIN" msg list --full | sed -n 's/.*nonce \([0-9a-z]*\).*/\1/p'); do
        say "• REPLY-$word"
      done ;;
    esac
  done
  say '● READY'
  draw idle
  promote() {
    shim="$state/members/$BORGCHART_MEMBER/bin/brg-slack"
    printf '{"tool_name":"Bash","tool_input":{"command":"%s promote %s question"}}' "$shim" "$1" | "$BORGCHART_BIN" _gate pre "$state" "$BORGCHART_MEMBER" || return 1
    "$shim" promote "$1" question || return 1
    if [ -n "$post" ] && [ "$scenario" != no-post ]; then
      "$BORGCHART_BIN" _gate post "$state" "$BORGCHART_MEMBER"
    fi
  }
  while :; do
    for input in "$inbox"/[0-9]*; do
      [ -f "$input" ] || continue
      body=$(cat "$input")
      rm "$input"
      draw working
      case $body in
        *'HOLD nonce '*)
          if [ -n "$post" ]; then promote 1700000000.000001; fi
          sleep 2 ;;
      esac
      case $body in
        *'Slack promote '*)
          ts=$(printf '%s' "$body" | sed -n 's/.*ts=\([^ ]*\).*/\1/p')
          say "● PROMOTED-$ts" ;;
        *'nonce '*)
          marker=•
          for word in $(printf '%s' "$body" | sed -n 's/.*nonce \([0-9a-z]*\).*/\1/p'); do
            [ "$scenario" = echo-reply ] && marker=›
            say "$marker REPLY-$word"
            marker=' '
          done ;;
        *'promote '*)
          ts=$(printf '%s' "$body" | sed -n 's/.*promote \([0-9.]*\).*/\1/p' | head -1)
          promote "$ts"
          say '● SENT' ;;
      esac
      if [ "$scenario" = late-idle ] && [ -z "${late_once:-}" ]; then
        late_once=1
        draw working
        sleep 1
        rm -f "$inbox"/[0-9]*
      fi
      if [ "$scenario" = late-idle ] && [ "${#body}" -ge 16000 ]; then
        touch "$fake/unfinished-$BORGCHART_MEMBER"
        draw working
        sleep 1
        rm "$fake/unfinished-$BORGCHART_MEMBER"
      fi
      draw idle
    done
    sleep 0.05
  done ;;
esac

nonce_word() { # the nonce from the inbox, read without marking it shown
  "$BORGCHART_BIN" msg list --member "$BORGCHART_MEMBER" --full | grep -o 'nonce [0-9a-z]*' | tail -1 | cut -d' ' -f2
}

if [ "$member" = typer ]; then
  stty raw -echo opost onlcr
  printf '\033[?2004h'
  draw idle
  while IFS= read -r -n 1 -d '' key; do
    case $key in
      $'\033')
        IFS= read -r -n 5 sequence
        IFS= read -r -d $'\033' buffer
        IFS= read -r -n 5 sequence
        buffer=$(printf '%s' "$buffer" | tr '\r' '\n')
        draw idle ;;
      $'\r'|$'\n')
        word=$(printf '%s' "$buffer" | sed -n 's/.*nonce \([0-9a-z]*\).*/\1/p')
        buffer=
        [ "$scenario" != drop-turn ] && say "● REPLY-$word"
        draw idle ;;
    esac
  done
fi

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
      "$BORGCHART_BIN" msg wait --timeout 600 >"$out" 2>&1 ;;
    early-turn|same-poll)
      BORGCHART_INCARNATION=not-this-one "$BORGCHART_BIN" msg wait >"$out" 2>&1 &
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
      "$BORGCHART_BIN" msg wait --timeout 600 >"$out" 2>&1 &
      say "● ARMED"
      draw idle
      wait $! ;;
  esac
  # The new turn, after the wait ended.
  sleep 0.3 # Keep the composer available long enough to expose accidental typing.
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
