#!/usr/bin/env bash
# How fast does a terminal consume PTY output?
#
# Each terminal is timed twice: once running only the marker touch, once
# running cat over the payload first. The difference is the time spent
# consuming output, which cancels out window and shell startup.
#
# Usage: scripts/throughput.sh [payload-megabytes]
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

run_once() { # $1 = terminal, $2 = shell command
  rm -f "$MARKER"
  local start end pid
  start=$(date +%s.%N)
  case "$1" in
    # A plain sh keeps a slow interactive rc file out of the measurement.
    tuni)
      SHELL=/bin/sh TUNI_CAPTURE_PNG=$WORK/frame.png \
        TUNI_CAPTURE_INPUT="$2"$'\n' TUNI_CAPTURE_DELAY_MS=120000 \
        "$TUNI" >/dev/null 2>&1 &
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
    *) "$1" -e sh -c "$2" >/dev/null 2>&1 & ;;
  esac
  pid=$!
  # 5ms, because the poll interval is the resolution of the whole benchmark:
  # at 50ms the fast terminals all landed in one bucket and read as a tie.
  for _ in $(seq 1 24000); do
    [ -e "$MARKER" ] && break
    sleep 0.005
  done
  end=$(date +%s.%N)
  kill "$pid" 2>/dev/null
  wait "$pid" 2>/dev/null
  [ -e "$MARKER" ] || return 1
  echo "$end - $start" | bc
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
  printf '%-10s startup %ss  total %ss  consume %ss  %s MiB/s\n' \
    "$term" "$base" "$full" "$delta" "$rate"
done
