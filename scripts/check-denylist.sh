#!/usr/bin/env bash
set -euo pipefail
export LC_ALL=C
cd -- "$(dirname -- "$0")/.."

p1="chess""bender"
p2="regi""cide"
p3="tarta""noglu"
p4="chess""[-_ ]""bender"
pattern="$p1|$p2|$p3|$p4"

files() {
    if [[ ${1:-} == --snapshot ]]; then
        # Nix's Git flake source is already filtered to tracked files.
        find . -type f -print0
    else
        git ls-files --cached --others --exclude-standard --deduplicate -z
    fi
}

scan() {
    local path status found=0
    while IFS= read -r -d '' path; do
        path="./${path#./}"
        [[ -f $path && ! -L $path ]] || continue
        if grep -aHniE -- "$pattern" "$path"; then
            found=1
        else
            status=$?
            [[ $status == 1 ]] || return "$status"
        fi
    done
    return "$found"
}

files "$@" | scan
echo "Denylist: 0 hits"
