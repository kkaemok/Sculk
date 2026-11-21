#!/bin/bash
set -euo pipefail
if [[ $# -lt 1 || $# -gt 2 ]]; then
    echo "Usage: $0 /absolute/path/to/sculk [--trace]" >&2
    exit 2
fi
binary=$(realpath "$1")
trace=
if [[ $# -eq 2 ]]; then
    trace=$2
    if [[ "$trace" != --trace ]]; then
        echo "Expected --trace or no second argument" >&2
        exit 2
    fi
fi
project=$(mktemp -d)
trap 'rm -rf -- "$project"' EXIT
awk 'BEGIN { for (n = 0; n < 2000; ++n) print "!count"; print ":quit" }' > "$project/input"
printf 'architecture=%s events=2000\n' "$(uname -m)"
for group in 1 32; do
    path="$project/group-$group"
    mkdir -p "$path"
    "$binary" --config "$path/sculk.toml" init >/dev/null
    sed -i "/^\\[storage\\]/a commit_every = $group" "$path/sculk.toml"
    sed -i 's/enabled = false/enabled = true/' "$path/sculk.toml"
    printf 'sqlite commit_every=%s\n' "$group"
    TIMEFORMAT='wall=%3Rs user=%3Us sys=%3Ss'
    if [[ "$trace" == --trace ]]; then
        time strace -f -c -e trace=fsync,fdatasync,openat,unlink,rename,write "$binary" --config "$path/sculk.toml" run --offline < "$project/input" > "$path/output"
    else
        time "$binary" --config "$path/sculk.toml" run --offline < "$project/input" > "$path/output"
    fi
    grep -Fx 'Bot (mock): Counter: 2000' "$path/output" >/dev/null
    printf '!count\n' | "$binary" --config "$path/sculk.toml" run --offline |
        grep -Fx 'Bot (mock): Counter: 2001' >/dev/null
done
