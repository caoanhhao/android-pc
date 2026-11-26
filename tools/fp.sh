#!/bin/bash
# Drive the virtual fingerprint sensor of aosp_pc_x86_64 over adb (docs/lockdown-biometric.md):
# Waydroid on this host, or the real PC through adb. Ported from waydroid-nvidia's
# fingerprint/fp.sh.
#
#   tools/fp.sh status
#   tools/fp.sh enroll [id]      enroll finger <id> (default: next free) through Settings
#   tools/fp.sh lock             lock the screen now (fingerprint stays allowed)
#   tools/fp.sh touch [id]       put enrolled finger <id> (default: first) on the sensor
#   tools/fp.sh bad              put an unknown finger on the sensor
#   tools/fp.sh lockout [on|off] lock fingerprint out (on: also touches once) / clear it
#
# Device: $ANDROID_SERIAL if set; else Waydroid when its container runs (adb to the IP from
# `waydroid status`); else adb's only device (the real PC over USB or `adb connect`).
# FP_PIN=1234 tools/fp.sh enroll   types the lock screen PIN when Settings asks for it.
#
# The sensor is an under-display one: a finger is a 0.8 s press on the sensor spot, which
# SystemUI hands to the HAL; the HAL then matches vendor.fingerprint.virtual.enrollment_hit
# against persist.vendor.fingerprint.virtual.enrollments.
set -euo pipefail
P=vendor.fingerprint.virtual
PP=persist.vendor.fingerprint.virtual

if [[ -z ${ANDROID_SERIAL:-} ]] && pgrep -f '[l]xc-start .* waydroid' >/dev/null; then
    ip=$(waydroid status 2>/dev/null | sed -n 's/^IP address:[[:space:]]*//p')
    [[ -n $ip ]] || { echo "Waydroid runs but has no IP in waydroid status yet" >&2; exit 1; }
    ANDROID_SERIAL=$ip:5555
    adb connect "$ANDROID_SERIAL" >/dev/null
    export ANDROID_SERIAL
fi
if ! adb shell true 2>/dev/null; then
    echo "adb cannot reach Android (${ANDROID_SERIAL:-no ANDROID_SERIAL}; adb devices:" \
        "$(adb devices | sed 1d | awk 'NF { printf "%s ", $1 }'))" >&2
    exit 1
fi
sh_() { adb shell "$@" | tr -d '\r'; }

enrolled() { sh_ getprop $PP.enrollments | tr ',' ' '; }
keyguard() { sh_ dumpsys window policy | sed -n 's/^ *showing=//p' | head -1; }

# StrongAuth flags of user 0, in hex, from SystemUI's keyguard dump (LockSettings' dump has
# them no longer).
strong_auth() {
    sh_ dumpsys activity service com.android.systemui/.SystemUIService KeyguardUpdateMonitor |
        sed -n 's/^ *strongAuthFlags=\([0-9a-f]*\).*/\1/p' | head -1
}

# RUNNING_LOCKED until the credential-encrypted storage is unlocked, then RUNNING_UNLOCKED.
user_state() { sh_ dumpsys user | sed -n 's/^ *State: //p' | head -1; }

# Where to press: the sensor centre from the HAL props, moved by the offset WM applies to
# SystemUI's overlay window when it does not fit on the display (Settings' enroll overlay
# can be larger than a small window, so WM clamps it and the hit area moves with it).
press() {
    local loc sx sy rest win req frm tries=6
    loc=$(sh_ getprop $PP.sensor_location)
    [[ -n $loc ]] || loc=400:1600:150 # the HAL's built-in default
    sx=${loc%%:*}; rest=${loc#*:}; sy=${rest%%:*}
    sh_ input keyevent KEYCODE_WAKEUP
    # SystemUI puts the sensor overlay up a moment after the lock screen starts listening
    [[ ${1:-} == quiet ]] && tries=1
    while :; do
        win=$(sh_ dumpsys window windows | awk '
            /Window #/ { inwin = /UdfpsControllerOverlay/ }
            inwin && /mAttrs=/ && !a { a = $0 }
            inwin && / frame=/ && !f { f = $0 }
            END { print a; print f }')
        req=$(sed -n '1s/.*mAttrs={(\(-\{0,1\}[0-9]*\),\(-\{0,1\}[0-9]*\)).*/\1 \2/p' <<<"$win")
        frm=$(sed -n '2s/.* frame=\[\(-\{0,1\}[0-9]*\),\(-\{0,1\}[0-9]*\)\].*/\1 \2/p' <<<"$win")
        [[ -z $req || -z $frm ]] || break
        tries=$((tries - 1))
        ((tries > 0)) || break
        sleep 0.5
    done
    if [[ -z $req || -z $frm ]]; then
        [[ ${1:-} == quiet ]] && return 0 # enroll: the overlay is not up yet, try again later
        echo "nothing is waiting for a finger (no fingerprint sensor on screen)." >&2
        local flags why=
        flags=$(strong_auth)
        if [[ -n $flags && $flags != 0 ]]; then
            (( (0x$flags & 0x01) == 0 )) || why="$why, Android just started"
            (( (0x$flags & 0x02) == 0 )) || why="$why, device admin lock"
            (( (0x$flags & 0x08) == 0 )) || why="$why, fingerprint lockout"
            (( (0x$flags & 0x10) == 0 )) || why="$why, no PIN for 72 h"
            echo "Android wants the PIN first (${why#, }; strong auth flags 0x$flags)." \
                "Unlock with it, then lock with: $0 lock" >&2
        elif [[ $(keyguard) != true ]]; then
            echo "The screen is not locked. Lock it with: $0 lock" >&2
        fi
        return 1
    fi
    read -r rx ry <<<"$req"; read -r fx fy <<<"$frm"
    sx=$((sx - rx + fx)); sy=$((sy - ry + fy))
    sh_ input swipe "$sx" "$sy" "$((sx + 1))" "$((sy + 1))" 800
}

# primary (bottom-right) button of the current Settings screen
tap_primary() {
    local xy
    sh_ uiautomator dump /sdcard/fp-ui.xml >/dev/null 2>&1 || return 0
    xy=$(sh_ cat /sdcard/fp-ui.xml | tr '>' '\n' | grep 'clickable="true"' |
        sed -n 's/.*bounds="\[\([0-9]*\),\([0-9]*\)\]\[\([0-9]*\),\([0-9]*\)\]".*/\1 \2 \3 \4/p' |
        awk '{ x = int(($1 + $3) / 2); y = int(($2 + $4) / 2)
               if (y > by || (y == by && x > bx)) { bx = x; by = y } }
             END { if (by) print bx, by }')
    [[ -z $xy ]] || sh_ input tap $xy
}

top_activity() {
    sh_ dumpsys activity activities |
        sed -n 's/.*topResumedActivity=ActivityRecord{[^ ]* [^ ]* \([^ }]*\).*/\1/p' | head -1
}

cmd_enroll() {
    local id=${1:-} asked= i=0 e
    if [[ -z $id ]]; then
        id=1
        for e in $(enrolled); do ((e < id)) || id=$((e + 1)); done
    fi
    # 5 captures of 300 ms each, then success; the HAL clears this once enrolled
    sh_ setprop $P.next_enrollment "$id:300,300,300,300,300:true"
    sh_ input keyevent KEYCODE_WAKEUP
    # fullscreen: in a desktop window Settings refuses ("Exit split screen to set up ...")
    sh_ am start --windowingMode 1 -a android.settings.FINGERPRINT_ENROLL >/dev/null
    echo "enrolling finger $id"
    while ((i < 40)); do
        i=$((i + 1))
        sleep 1.5
        case $(top_activity) in
        *ConfirmLock*)
            [[ -z $asked ]] || continue
            asked=1
            if [[ -n ${FP_PIN:-} ]]; then
                sh_ input text "$FP_PIN"
                sh_ input keyevent KEYCODE_ENTER
            else
                echo "enter the lock screen PIN on the device (or set FP_PIN)"
            fi ;;
        *FingerprintEnrollIntroduction | *FingerprintEnrollFindSensor)
            tap_primary ;; # More, I agree, Start
        *FingerprintEnrollEnrolling)
            press quiet ;;
        *FingerprintEnrollFinish)
            tap_primary # Done
            break ;;
        *ChooseLock*)
            echo "Android has no screen lock: set a PIN first (Settings > Security, or" \
                "adb shell locksettings set-pin <PIN>)" >&2
            exit 1 ;;
        esac
    done
    case " $(enrolled) " in
    *" $id "*) echo "enrolled: $(enrolled)" ;;
    *) echo "finger $id was not enrolled (enrollments: $(enrolled))" >&2; exit 1 ;;
    esac
}

cmd_touch() {
    local id=${1:-} state
    [[ -n $id ]] || id=$(enrolled | awk '{ print $1 }')
    [[ -n $id ]] || { echo "no finger is enrolled; run: $0 enroll" >&2; exit 1; }
    sh_ setprop $P.enrollment_hit "$id"
    press
    # a press right after other input on the lock screen can miss the sensor: press once more
    sleep 1
    [[ $(keyguard) != true ]] || press
    # After a boot, the remembered PIN unlocks the user's storage next
    # (tools/patches/aosp17-boot-biometric-unlock.patch); that takes a moment.
    state=$(user_state)
    if [[ $state == RUNNING_LOCKED ]]; then
        sleep 3
        state=$(user_state)
    fi
    sleep 0.5
    echo "keyguard: $(keyguard | sed 's/true/showing/; s/false/hidden/'), user 0: $state"
    [[ $state != RUNNING_LOCKED ]] ||
        echo "the storage is still locked: no PIN was remembered yet (enter it once)" >&2
}

cmd_bad() {
    local hit
    hit=$(sh_ getprop $P.enrollment_hit)
    sh_ setprop $P.enrollment_hit 0
    press || { sh_ setprop $P.enrollment_hit "${hit:-0}"; return 1; }
    sleep 1
    # waydroid.cfg sets the hit to the enrolled finger, so that a real touch matches: put it back
    sh_ setprop $P.enrollment_hit "${hit:-0}"
    echo "keyguard: $(keyguard | sed 's/true/showing/; s/false/hidden/')"
}

cmd_lockout() {
    case ${1:-on} in
    on)
        sh_ setprop $PP.lockout true
        press # the HAL reports lockout to whoever is listening
        echo "locked out; unlocking with the PIN clears it" ;;
    off) sh_ setprop $PP.lockout false ;;
    *) echo "usage: $0 lockout [on|off]" >&2; exit 1 ;;
    esac
}

# Lock now, keeping fingerprint allowed (the power menu's Lockdown also allows it on this
# product, docs/lockdown-biometric.md, but ends only with an unlock). The screen going off locks
# only after "lock after screen timeout", so call IWindowManager.lockNow(null); its binder
# transaction code (TRANSACTION_lockNow) changes between Android versions, hence the SDK check.
cmd_lock() {
    case $(sh_ getprop ro.build.version.sdk) in
    37) sh_ service call window 75 i32 0 >/dev/null ;;
    *)
        sh_ input keyevent KEYCODE_SLEEP
        sleep 1
        sh_ input keyevent KEYCODE_WAKEUP ;;
    esac
    sleep 0.5
    [[ $(keyguard) == true ]] || { echo "the screen did not lock" >&2; exit 1; }
}

cmd_status() {
    echo "device:      ${ANDROID_SERIAL:-$(adb get-serialno)} ($(sh_ wm size | sed -n 's/.*size: //p' | tail -1))"
    echo "type:        $(sh_ getprop $PP.type)  sensor at $(sh_ getprop $PP.sensor_location)"
    echo "enrollments: $(enrolled)"
    echo "finger hit:  $(sh_ getprop $P.enrollment_hit)"
    echo "lockout:     $(sh_ getprop $PP.lockout)"
    echo "keyguard:    $(keyguard | sed 's/true/showing/; s/false/hidden/')"
    echo "strong auth: 0x$(strong_auth)"
    echo "user 0:      $(user_state)"
    sh_ dumpsys fingerprint | sed -n 's/^Dumping for sensorId: /sensor id:   /p; /"prints"/p; s/^Current operation: /busy:        /p'
}

case "${1:-}" in
lock | enroll | touch | bad | lockout | status) c=$1; shift; "cmd_$c" "$@" ;;
*) sed -n '2,19p' "$0" | sed 's/^# \{0,1\}//'; exit 1 ;;
esac
