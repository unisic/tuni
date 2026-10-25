#!/usr/bin/env bash
# How fast does a terminal consume PTY output?
#
# Each terminal is timed twice: once running only the marker touch, once
# running cat over the payload first. The difference is the time spent
# consuming output, which cancels out window and shell startup. A third run
# leaves the shell sleeping and reads what the terminal costs while it waits:
# its memory three seconds after the marker, and the CPU it spends in the five
# seconds after that.
#
# Usage: scripts/throughput.sh [payload-megabytes] [terminal...]
set -u
export LC_ALL=C

MB=${1:-200}
ROOT=$(cd "$(dirname "$0")/.." && pwd)
TUNI=$ROOT/target/release/tuni
WORK=${TMPDIR:-/tmp}/tuni-throughput
PAYLOAD=$WORK/payload.txt
MARKER=$WORK/done
RUNS=${RUNS:-2}

[ -x "$TUNI" ] || { echo "build first: cargo build --release"; exit 1; }
mkdir -p "$WORK"

# tuni has no -e. Its pane runs $SHELL, so the command rides in as a shell
# that hands it straight to sh -c, which is what -e does for the others.
SHIM=$WORK/shell
printf '#!/bin/sh\nexec /bin/sh -c "$TUNI_BENCH_COMMAND"\n' >"$SHIM"
chmod +x "$SHIM"

if [ ! -e "$PAYLOAD" ] || [ "$(stat -c%s "$PAYLOAD")" -lt $((MB * 1048576)) ]; then
  echo "generating ${MB}MiB payload…"
  # Mixed plain and SGR-colored lines, so the parser does real work rather
  # than memcpy.
  python3 - "$PAYLOAD" "$MB" <<'PY'
import random, sys
path, mb = sys.argv[1], int(sys.argv[2])
random.seed(7)
words = ['alpha','beta','gamma','delta','epsilon','zeta','eta','theta']
target = mb * 1024 * 1024
with open(path, 'w') as f:
    written, i = 0, 0
    while written < target:
        line = ' '.join(random.choice(words) for _ in range(10))
        if i % 5 == 0:
            line = '\033[1;3%dm%s\033[0m' % (i % 8, line)
        f.write(line + '\n')
        written += len(line) + 1
        i += 1
PY
fi

launch() { # $1 = terminal, $2 = shell command; leaves the pid in $!
  case "$1" in
    # TUNI_SESSION=0 keeps a restored session out of the run, and a process of
    # its own keeps it off a tuni that is already open.
    tuni)
      TUNI_SESSION=0 TUNI_SINGLE_INSTANCE=0 TUNI_BENCH_COMMAND="$2" \
        SHELL=$SHIM "$TUNI" >/dev/null 2>&1 &
      ;;
    # kitty and foot spell the command as trailing arguments; -e is either
    # ignored or an error depending on the version.
    kitty|foot) "$1" sh -c "$2" >/dev/null 2>&1 & ;;
    wezterm) wezterm start -- sh -c "$2" >/dev/null 2>&1 & ;;
    # -e here takes one string and stops, so the command needs -x instead.
    xfce4-terminal|terminator) "$1" -x sh -c "$2" >/dev/null 2>&1 & ;;
    # These hand the window to a server and return, so without waiting the
    # timer would stop before the window has read a byte.
    gnome-terminal) gnome-terminal --wait -- sh -c "$2" >/dev/null 2>&1 & ;;
    ptyxis) ptyxis --standalone -- sh -c "$2" >/dev/null 2>&1 & ;;
    # kerod owns the PTY and the open app draws it, so the session goes to the
    # app already running with -f. A directory outside every project would
    # leave a new project behind in the sidebar; home lands in the one kero
    # starts with.
    kero) kero new -f -w -d "$HOME" -- sh -c "$2" >"$WORK/kero.id" 2>/dev/null & ;;
    *) "$1" -e sh -c "$2" >/dev/null 2>&1 & ;;
  esac
}

finish() { # $1 = terminal, $2 = pid
  kill "$2" 2>/dev/null
  wait "$2" 2>/dev/null
  # The session outlives its command in a shell, so without this every run
  # would leave one more session in the app.
  [ "$1" = kero ] && kero close "$(cat "$WORK/kero.id")" >/dev/null 2>&1
  [ -e "$MARKER" ]
}

await_marker() {
  # 5ms, because the poll interval is the resolution of the whole benchmark:
  # at 50ms the fast terminals all landed in one bucket and read as a tie.
  for _ in $(seq 1 24000); do
    [ -e "$MARKER" ] && break
    sleep 0.005
  done
}

run_once() { # $1 = terminal, $2 = shell command
  rm -f "$MARKER"
  local start end pid
  start=$(date +%s.%N)
  launch "$1" "$2"
  pid=$!
  await_marker
  end=$(date +%s.%N)
  finish "$1" "$pid" || return 1
  echo "$end - $start" | bc
}

ticks() { awk '{print $14 + $15}' "/proc/$1/stat" 2>/dev/null; }

# RSS and PSS in MiB, then CPU milliseconds over five idle seconds. Only the
# process that was started is read, so a terminal that hands its window to a
# server (kero, gnome-terminal, ptyxis) reads n/a rather than the client.
footprint() { # $1 = terminal
  rm -f "$MARKER"
  local pid mem before after hz
  launch "$1" "touch $MARKER; exec sleep 30"
  pid=$!
  await_marker
  # The marker says the shell started, not that the window is done: a GTK
  # window still paints its first frames and answers the compositor's
  # configure for a second or two after that. Read sooner, and the idle
  # column measures the end of startup rather than a terminal waiting.
  sleep 3
  mem=$(awk '/^Rss:/ {r = $2} /^Pss:/ {p = $2}
    END {if (r) printf "%.1f %.1f", r / 1024, p / 1024}' \
    "/proc/$pid/smaps_rollup" 2>/dev/null)
  before=$(ticks "$pid")
  sleep 5
  after=$(ticks "$pid")
  hz=$(getconf CLK_TCK)
  finish "$1" "$pid" || return 1
  [ -n "$mem" ] && [ -n "$before" ] && [ -n "$after" ] || { echo "n/a n/a n/a"; return; }
  echo "$mem $(( (after - before) * 1000 / hz ))"
}

best() { # $1 = terminal, $2 = command
  local min="" t
  for _ in $(seq 1 $RUNS); do
    t=$(run_once "$1" "$2") || return 1
    if [ -z "$min" ] || (( $(echo "$t < $min" | bc -l) )); then min=$t; fi
    sleep 1
  done
  echo "$min"
}

bytes=$(stat -c%s "$PAYLOAD")
printf 'payload: %s bytes\n\n' "$bytes"

for term in tuni "$@"; do
  case "$term" in [0-9]*) continue ;; esac
  [ "$term" = tuni ] || command -v "$term" >/dev/null || continue
  base=$(best "$term" "touch $MARKER") || { echo "$term: failed"; continue; }
  full=$(best "$term" "cat $PAYLOAD; touch $MARKER") || { echo "$term: failed"; continue; }
  delta=$(echo "$full - $base" | bc)
  rate=$(echo "scale=1; $bytes / 1048576 / $delta" | bc -l)
  read -r rss pss idle < <(footprint "$term" || echo "n/a n/a n/a")
  printf '%-10s startup %ss  total %ss  consume %ss  %s MiB/s  rss %s MiB  pss %s MiB  idle %s ms/5s\n' \
    "$term" "$base" "$full" "$delta" "$rate" "$rss" "$pss" "$idle"
done
