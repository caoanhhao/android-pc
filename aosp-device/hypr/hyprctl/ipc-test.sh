#!/system/bin/sh
# ipc-test.sh: exercises every hyprctl request and provokes the event socket's events on a
# running image (Phase E12, docs/hypr-ipc.md). Run it as root or shell on TEST DATA only: it
# opens and kills apps (Settings, Files, Clock), switches workspaces, makes and removes an
# overlay display and changes gaps_in for a moment. Prints PASS/FAIL/SKIP lines and a summary.
#
#   sudo waydroid shell -- sh /data/local/tmp/ipc-test.sh
#   adb shell sh /data/local/tmp/ipc-test.sh

H=${HYPRCTL:-hyprctl}
EV=/data/local/tmp/hypr-ipc-events.txt
pass=0
fail=0

ok() { echo "PASS $1"; pass=$((pass + 1)); }
bad() { echo "FAIL $1: $2"; fail=$((fail + 1)); }

# expect NAME PATTERN ARGS...: hyprctl ARGS must print something matching PATTERN (grep -E).
expect() {
    name=$1; pat=$2; shift 2
    out=$($H "$@" 2>&1)
    if echo "$out" | grep -qE -- "$pat"; then ok "$name"; else bad "$name" "$(echo "$out" | head -3 | tr '\n' '|')"; fi
}

# A window's address by its class (package), from `clients`.
addr_of() {
    $H clients | awk -v c="$1" '/^Window /{a="0x"$2} $1=="class:" && $2==c {print a; exit}'
}

settle() { sleep "${1:-1.5}"; }

: > "$EV"
$H events > "$EV" 2>&1 &
EVPID=$!
settle 1

echo "== requests"
expect version '^Hyprland 0\.57\.0 built from branch android-pc' version
expect "version -j" '"version": "0.57.0"' -j version
expect splash '.' splash
expect instances '^instance android:' instances
expect "instances -j" '"instance": "android"' -j instances
expect layouts '^dwindle$' layouts
expect submap '^default$' submap
expect locked '^(true|false)$' locked
expect "locked -j" '"locked": (true|false)' -j locked
expect globalshortcuts '^\[\]$' -j globalshortcuts
expect monitors '^Monitor DISPLAY-0 \(ID 0\):' monitors
expect "monitors all" '^Monitor DISPLAY-0' monitors all
expect "monitors -j" '"activeWorkspace": \{' -j monitors
expect devices '^mice:' devices
expect "devices -j" '"keyboards": ' -j devices
expect binds '^bind' binds
expect "binds -j" '"modmask": ' -j binds
expect layers 'namespace: bar' layers
expect "layers -j" '"levels": \{' -j layers
expect cursorpos '^-?[0-9]+, -?[0-9]+$' cursorpos
expect "cursorpos -j" '"x": ' -j cursorpos
expect animations '^animations:' animations
expect "animations -j" '"bezier": ' -j animations
expect workspacerules '.*' workspacerules
expect "workspacerules -j" '^\[' -j workspacerules
expect descriptions '"general:gaps_in"' descriptions
expect configerrors '.*' configerrors
expect "getoption" '^int: [0-9]+$' getoption general:gaps_in
expect "getoption -j" '"option": "general:gaps_in"' -j getoption general:gaps_in
old_gaps=$($H getoption general:gaps_in | sed -n 's/^int: //p')
expect keyword '^ok$' keyword general:gaps_in 9
settle 1
expect "keyword took" '^int: 9$' getoption general:gaps_in
$H keyword general:gaps_in "${old_gaps:-5}" >/dev/null
expect "keyword bad value" '^error' keyword general:gaps_in lots
expect eval '^ok$' eval 'x_ipc = 6 * 7'
expect repl '^42$' repl 'x_ipc'
expect "repl print" '^a	1$' repl "print('a', 1)"
expect "eval error" '^error: ' eval 'error("boom")'
expect setcursor '^error' setcursor Bibata 24
expect plugin '^error' plugin load x.so
expect "unknown request" '^unknown request$' nosuchrequest
expect rollinglog '.' rollinglog
expect notify '^ok$' notify 1 3000 "rgb(89b4fa)" "hyprctl notify from ipc-test"
expect "notify fontsize" '^ok$' notify -1 3000 0 "fontsize:20 big text"
expect dismissnotify '^ok$' dismissnotify 1
expect "dismissnotify all" '^ok$' dismissnotify
expect seterror '^ok$' seterror 'rgba(66ee66ff)' hello from hyprctl
settle 1
expect "seterror disable" '^ok$' seterror disable

echo "== windows and dispatchers"
expect "dispatch workspace" '^ok$' dispatch workspace 2
settle
expect "dispatch exec settings" '^ok$' dispatch exec com.android.settings
settle 3
expect "dispatch exec files" '^ok$' dispatch exec com.android.documentsui
settle 3
expect clients '^Window [0-9a-f]+ -> ' clients
expect "clients -j" '"focusHistoryID": ' -j clients
expect "clients -j fields" '"workspace": \{' -j clients
expect activewindow 'class: com.android.documentsui' activewindow
expect "activewindow -j" '"class": "com.android.documentsui"' -j activewindow
expect activeworkspace '^workspace ID 2 \(2\)' activeworkspace
expect workspaces 'workspace ID 2 \(2\) on monitor DISPLAY-0' workspaces
expect "workspaces -j" '"tiledLayout": ' -j workspaces
S=$(addr_of com.android.settings)
F=$(addr_of com.android.documentsui)
echo "   settings=$S files=$F"
expect "focuswindow address" '^ok$' dispatch focuswindow "address:$S"
settle
expect "focuswindow took" 'class: com.android.settings' activewindow
expect "focuswindow class" '^ok$' dispatch focuswindow class:documentsui
settle
expect togglefloating '^ok$' dispatch togglefloating
settle
expect "floating set" 'floating: 1' activewindow
expect pin '^ok$' dispatch pin
settle
expect "pinned set" 'pinned: 1' activewindow
expect "getprop pinned" '^true$' getprop activewindow pinned
expect setprop '^ok$' setprop activewindow alpha 0.8
expect "getprop alpha" '^0\.8' getprop activewindow alpha
expect "setprop unset" '^ok$' setprop activewindow alpha unset
expect "setprop unknown" '^prop not found$' setprop activewindow nosuchprop 1
expect "pin off" '^ok$' dispatch pin
settle
expect "togglefloating back" '^ok$' dispatch togglefloating
settle
expect fullscreen '^ok$' dispatch fullscreen 0
settle 2
expect "fullscreen set" 'fullscreen: 2' activewindow
expect "fullscreen off" '^ok$' dispatch fullscreen 0
settle 2
expect "movetoworkspacesilent window" '^ok$' dispatch movetoworkspacesilent "3,address:$S"
settle 2
expect "moved" 'workspace: 3 \(3\)' clients
expect "dispatch Lua" '^ok$' dispatch 'hl.dsp.focus({ workspace = 3 })'
settle 2
expect "Lua workspace took" '^workspace ID 3 ' activeworkspace
expect togglespecialworkspace '^ok$' dispatch togglespecialworkspace
settle 2
expect "special shown" 'special workspace: -99 \(special:special\)' monitors
expect "togglespecialworkspace off" '^ok$' dispatch togglespecialworkspace
settle 2
expect cyclenext '^ok$' dispatch cyclenext
expect focusurgentorlast '^ok$' dispatch focusurgentorlast
expect "bad dispatcher" '^Invalid dispatcher' dispatch nosuchdispatcher
expect "bad window" '^Window not found$' dispatch closewindow class:no.such.app
expect batch '^ok' --batch "dispatch workspace 2 ; j/activeworkspace"
expect "batch -j" '"id": [0-9]+' -j --batch "activeworkspace; dispatch workspace 2"
expect execr '^ok$' dispatch execr "$H notify 5 2000 0 from-execr"
expect switchxkblayout '^(ok|error)' switchxkblayout current next
expect "output create" '^ok$' output create headless
settle 4
expect "output listed" 'Monitor DISPLAY-[1-9]' monitors
OUT=$($H monitors | sed -n 's/^Monitor \(DISPLAY-[1-9][0-9]*\) .*/\1/p' | tail -1)
expect "output remove" '^ok$' output remove "${OUT:-DISPLAY-1}"
settle 3
expect reload '^ok$' reload
settle 3
expect decorations '^Decoration: Border' decorations activewindow
# kill mode: the next tap on Files' window force-stops it (Files brought to front first).
$H dispatch focuswindow class:documentsui >/dev/null
settle 2
expect kill '^ok$' kill
set -- $($H -j clients | tr -d ' \n' | sed 's/},{/}\n{/g' | grep '"class":"com.android.documentsui"' | sed -n 's/.*"at":\[\([0-9-]*\),\([0-9-]*\)\],"size":\[\([0-9]*\),\([0-9]*\)\].*/\1 \2 \3 \4/p')
if [ -n "$1" ]; then
    input tap $(($1 + $3 / 2)) $(($2 + $4 / 2))
    settle 3
    if $H clients | grep -q 'class: com.android.documentsui'; then bad "kill mode" "Files still open"; else ok "kill mode"; fi
else
    bad "kill mode" "no Files window to click"
fi
expect closewindow '^ok$' dispatch closewindow "address:$S"
settle 3

echo "== events"
kill $EVPID 2>/dev/null
for e in workspace workspacev2 createworkspace createworkspacev2 destroyworkspace destroyworkspacev2 \
    activewindow activewindowv2 openwindow closewindow movewindow movewindowv2 changefloatingmode \
    pin fullscreen activespecial activespecialv2 configreloaded kill monitoradded monitoraddedv2 \
    monitorremoved monitorremovedv2; do
    line=$(grep -m1 "^$e>>" "$EV")
    if [ -n "$line" ]; then ok "event $line"; else bad "event $e" "not seen"; fi
done
for e in focusedmon urgent minimized windowtitle submap openlayer closelayer screencast activelayout; do
    line=$(grep -m1 "^$e>>" "$EV")
    if [ -n "$line" ]; then echo "SEEN event $line"; else echo "SKIP event $e (not provoked here, docs/hypr-ipc.md)"; fi
done
echo "== $pass passed, $fail failed (events in $EV)"
[ $fail -eq 0 ]
