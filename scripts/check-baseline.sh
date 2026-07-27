#!/bin/sh
# Refuses a binary that was compiled for the machine that built it.
#
# The Zig half of the build takes its CPU from a flag the Makefile passes
# through a shim on PATH, and a cached artifact walks straight past both. So
# the binary is asked directly instead of being trusted: 1.1.1 shipped a memset
# full of AVX-512 because a GitHub runner had it, and every machine without it
# died on SIGILL before the window appeared.
#
# Vector code that chooses itself at runtime is not the problem and is what the
# list below allows. simdutf, memchr, simd-adler32, the Highway kernels
# Ghostty carries, and aho-corasick's Teddy searcher (behind fancy-regex, which
# matches URLs) all ask CPUID first and have a fallback behind them. A ymm
# anywhere else got there because something compiled for the build machine.
#
# That question can only be asked of a binary that still has its symbols. Every
# package strips, and objdump then files the whole program under `.text`, so
# what is left there is the AVX-512 count: zero in a baseline build, hundreds in
# one made on a runner, and enough to catch the thing that shipped in 1.1.1.
# CI runs this on the build it just made, with symbols, for the rest.
set -eu

binary=${1:?usage: check-baseline.sh BINARY}
dispatched='simdutf|memchr|simd_adler32|N_AVX2|N_AVX3|hwy|aho_corasick.*teddy'

if [ "$(uname -m)" != x86_64 ]; then
    echo "$(uname -m) has no baseline to check against"
    exit 0
fi

# AVX-512 has no runtime dispatch here at all, so any of it is a mistake.
avx512=$(objdump -d "$binary" | grep -cE '%zmm|\bkmov[bwdq]\b' || true)
stray=
if readelf -S "$binary" | grep -q '\.symtab'; then
    stray=$(objdump -d "$binary" \
        | awk '/^[0-9a-f]+ </ { symbol = $2 } /%[yz]mm/ { print symbol }' \
        | grep -vE "$dispatched" | sort -u || true)
fi

if [ "$avx512" -ne 0 ] || [ -n "$stray" ]; then
    echo "$binary was not built for the baseline CPU:" >&2
    [ "$avx512" -eq 0 ] || echo "  AVX-512 in $avx512 places" >&2
    [ -z "$stray" ] || printf '  AVX outside runtime dispatch:\n%s\n' "$stray" >&2
    echo "Build it with \`make build\`, which pins the CPU, and if a new" \
        "library dispatches on CPUID add it to this script." >&2
    exit 1
fi

echo "$binary is baseline x86_64"
