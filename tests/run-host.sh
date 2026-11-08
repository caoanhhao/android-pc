#!/usr/bin/env bash
# Build the test programs and run them on the Linux host, which acts as the
# oracle: every test must pass here before it is used against the Rust kernel.
#
#   tests/run-host.sh [-v] [FILTER...]
#
# FILTERs are name prefixes (e.g. "b1", "b4_unix"). -v prints every line of
# test output instead of only failures. Logs go to tests/target/ktest-logs/.
# Exit status: 0 if every selected test passed.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
verbose=0
filters=()
for a in "$@"; do
    case "$a" in
        -v) verbose=1 ;;
        -h|--help) sed -n '2,10p' "$0"; exit 0 ;;
        *) filters+=("$a") ;;
    esac
done

# cargo reads .cargo/config.toml from the cwd, so build from tests/.
(cd "$here" && cargo build --release --quiet)
bindir="$here/target/x86_64-unknown-linux-gnu/release"
logdir="$here/target/ktest-logs"
mkdir -p "$logdir"

selected() {
    [ ${#filters[@]} -eq 0 ] && return 0
    local f
    for f in "${filters[@]}"; do [[ "$1" == "$f"* ]] && return 0; done
    return 1
}

# Check one test's output against the protocol in tests/README.md.
# Args: name status logfile. Prints a reason and returns 1 on violation.
validate() {
    local name="$1" status="$2" log="$3"
    awk -v name="$name" -v status="$status" '
        /^#/ || /^$/ { next }
        NR == 1 && $0 != "BEGIN " name { print "first line is not BEGIN " name; bad = 1; exit }
        $1 == "BEGIN" { begins++; next }
        $1 == "PASS" { if ($2 != name) { print "PASS for wrong test: " $0; bad = 1 } pass++; next }
        $1 == "FAIL" { fail++; next }
        $1 == "SKIP" { next }
        $1 == "END" { end = $0; ends++; next }
        { print "unexpected line: " $0; bad = 1 }
        END {
            if (bad) exit 1
            if (ends != 1 || begins != 1) { print "need exactly one BEGIN and one END"; exit 1 }
            want = "END " name " pass=" pass + 0 " fail=" fail + 0
            if (end != want) { print "END line \"" end "\" does not match counted \"" want "\""; exit 1 }
            if (fail > 0) { print fail " case(s) failed"; exit 1 }
            if (status != 0) { print "exit status " status; exit 1 }
            if (pass == 0) { print "no cases ran"; exit 1 }
        }' "$log"
}

total=0
failed=()
while read -r name; do
    [[ -z "$name" || "$name" == \#* ]] && continue
    selected "$name" || continue
    total=$((total + 1))
    log="$logdir/$name.log"
    status=0
    # Run from a scratch cwd so tests never depend on the repo layout.
    (cd /tmp && timeout 120 "$bindir/$name" </dev/null >"$log" 2>&1) || status=$?
    if reason="$(validate "$name" "$status" "$log")"; then
        printf 'ok    %-14s %s\n' "$name" "$(grep '^END' "$log" | cut -d' ' -f3-)"
        [ $verbose -eq 1 ] && sed 's/^/      /' "$log"
    else
        printf 'FAIL  %-14s %s\n' "$name" "$reason"
        if [ $verbose -eq 1 ]; then sed 's/^/      /' "$log"; else grep -E '^(FAIL|#)' "$log" | sed 's/^/      /' || true; fi
        failed+=("$name")
    fi
done <"$here/tests.list"

# The in-guest runner must agree when it drives the same binaries.
if [ ${#filters[@]} -eq 0 ]; then
    total=$((total + 1))
    log="$logdir/ktest-init.log"
    status=0
    (cd /tmp && timeout 600 "$bindir/ktest-init" "$bindir" </dev/null >"$log" 2>&1) || status=$?
    end="$(grep '^END ktest-init' "$log" || true)"
    if [ "$status" -eq 0 ] && [[ "$end" == *" fail=0" ]]; then
        printf 'ok    %-14s %s\n' "ktest-init" "${end#END ktest-init }"
    else
        printf 'FAIL  %-14s exit %s, %s\n' "ktest-init" "$status" "${end:-no END line}"
        failed+=("ktest-init")
    fi
fi

echo
if [ ${#failed[@]} -eq 0 ]; then
    echo "ALL PASS ($total programs)"
else
    echo "FAILED: ${failed[*]} (${#failed[@]}/$total programs)"
    exit 1
fi
