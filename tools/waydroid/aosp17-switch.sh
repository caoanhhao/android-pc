#!/bin/bash
# Switch the host's Waydroid between its LineageOS 20 image and this repo's AOSP 17 image
# (docs/waydroid-aosp17.md). Run as your user, not with sudo: it asks for the password itself.
#
#   aosp17-switch.sh backup             stop Waydroid and back up its userdata, images,
#                                       overlay, waydroid.cfg and props
#   aosp17-switch.sh install DIR        put DIR/system.img and DIR/vendor.img (the PC build,
#                                       packed by aosp17-pack.sh) in images-aosp17/ and switch
#   aosp17-switch.sh use aosp17|lineage20
#                                       switch images, overlay and [properties]
#   aosp17-switch.sh restore BACKUP     put a backup's userdata back (lineage20 needs this:
#                                       Android cannot boot an older system on upgraded data)
#   aosp17-switch.sh status
#
# Layout under /var/lib/waydroid: images/ (LineageOS 20, never written here), images-aosp17/,
# overlay/ + overlay_rw/ + overlay_work/ of the active image and overlay.<name>/... of the
# other one, properties.<name> (the [properties] of waydroid.cfg per image), active-image.
set -euo pipefail

WD=/var/lib/waydroid
DATA=${WAYDROID_DATA:-$HOME/.local/share/waydroid/data}
BACKUPS=${WAYDROID_BACKUPS:-$HOME/.local/share/waydroid-backup}
SUDO=sudo
[ "$(id -u)" = 0 ] && SUDO=

die() { echo "aosp17-switch: $*" >&2; exit 1; }
active() { cat "$WD/active-image" 2>/dev/null || echo lineage20; }

stop_waydroid() {
    waydroid session stop >/dev/null 2>&1 || true
    $SUDO waydroid container stop >/dev/null 2>&1 || true
    local state
    state=$($SUDO lxc-info -P "$WD/lxc" -n waydroid -sH 2>/dev/null || echo STOPPED)
    [ "$state" = STOPPED ] || die "the container is still $state"
}

# cfg_props save NAME: [properties] of waydroid.cfg -> properties.NAME
# cfg_props load NAME IMAGES_PATH: properties.NAME -> [properties], images_path
cfg_props() {
    $SUDO python3 -I - "$WD" "$@" <<'EOF'
import configparser, sys
wd, op, name = sys.argv[1:4]
cfg = configparser.ConfigParser()
cfg.optionxform = str
cfg.read(wd + "/waydroid.cfg")
# strict=False: a key set twice (the user's value, then the image's) takes the last value.
props = configparser.ConfigParser(strict=False)
props.optionxform = str
if op == "save":
    props["properties"] = dict(cfg["properties"])
    with open("%s/properties.%s" % (wd, name), "w") as f:
        props.write(f)
else:
    props.read("%s/properties.%s" % (wd, name))
    cfg["properties"] = dict(props["properties"])
    cfg["waydroid"]["images_path"] = sys.argv[4]
    with open(wd + "/waydroid.cfg", "w") as f:
        cfg.write(f)
EOF
}

images_of() { case $1 in lineage20) echo "$WD/images";; aosp17) echo "$WD/images-aosp17";; *) die "unknown image $1";; esac; }

use() {
    local to=$1 from
    from=$(active)
    images_of "$to" >/dev/null
    [ -f "$(images_of "$to")/system.img" ] || die "no $(images_of "$to")/system.img"
    [ -f "$WD/properties.$to" ] || die "no $WD/properties.$to"
    stop_waydroid
    if [ "$from" != "$to" ]; then
        cfg_props save "$from"
        for d in overlay overlay_rw overlay_work; do
            if [ -e "$WD/$d.$from" ]; then
                # Moved by an earlier run that stopped before switching: $d is $to's already.
                [ -e "$WD/$d.$to" ] && die "both $WD/$d.$from and $WD/$d.$to exist"
                continue
            fi
            [ -e "$WD/$d" ] && $SUDO mv "$WD/$d" "$WD/$d.$from"
            [ -e "$WD/$d.$to" ] && $SUDO mv "$WD/$d.$to" "$WD/$d"
        done
        $SUDO mkdir -p "$WD/overlay" "$WD/overlay_rw" "$WD/overlay_work"
    fi
    if [ "$to" = aosp17 ]; then
        # Waydroid bind-mounts its waydroid.prop over /vendor/waydroid.prop and needs the
        # file to exist. The PC's vendor image has none (its presence is what tells the
        # image it runs in Waydroid), so the overlay provides it.
        $SUDO mkdir -p "$WD/overlay/vendor"
        $SUDO touch "$WD/overlay/vendor/waydroid.prop"
        # Mount points of Waydroid's LXC config (tmpfs) the PC's root has no directory for.
        $SUDO mkdir -p "$WD/overlay/mnt_extra" "$WD/overlay/var" "$WD/overlay/run"
    fi
    cfg_props load "$to" "$(images_of "$to")"
    echo "$to" | $SUDO tee "$WD/active-image" >/dev/null
    $SUDO waydroid upgrade --offline
    # `waydroid upgrade` rewrites the LXC config from its templates: the container guard
    # must have come with it (tools/waydroid/container-guard.sh).
    "$(dirname "$0")/container-guard.sh" check ||
        die "the container guard is missing: run tools/waydroid/container-guard.sh install"
    # The container service is not started here (CLAUDE.md, "Waydroid rule"); `waydroid
    # upgrade` D-Bus-activates it even when it is disabled, so stop it again.
    $SUDO systemctl stop waydroid-container
    echo "Waydroid now uses $to ($(images_of "$to")); not started."
}

backup() {
    [ "$(active)" = lineage20 ] || die "backup is for the LineageOS 20 state; active is $(active)"
    local b
    b=$BACKUPS/lineage20-$(date +%Y%m%d-%H%M)
    stop_waydroid
    local need have
    need=$($SUDO du -sxBG --exclude=./media "$DATA" | cut -f1 | tr -dc 0-9)
    have=$(df -BG --output=avail / | tail -1 | tr -dc 0-9)
    (( have > need + 20 )) || die "need ${need} GB (+20 GB headroom) on /, have ${have} GB"
    $SUDO mkdir -p "$b/var"
    echo "Backing up $DATA to $b/data (media/ hard-linked) ..."
    $SUDO rsync -aHAX --exclude=/media "$DATA/" "$b/data/"
    # Shared storage (game downloads, 50+ GB) as hard links: no extra space; protects against
    # deletion, not against in-place writes. The system upgrade does not touch it.
    $SUDO cp -al "$DATA/media" "$b/data/media"
    echo "Backing up $WD (images, overlay, cfg, props) to $b/var ..."
    $SUDO cp -a "$WD/images" "$WD/overlay" "$WD/waydroid.cfg" "$WD/waydroid_base.prop" \
        "$WD/waydroid.prop" "$b/var/"
    [ -d "$WD/overlay_rw" ] && $SUDO cp -a "$WD/overlay_rw" "$b/var/"
    echo "Backup done: $b"
}

restore() {
    local b=$1
    [ -d "$b/data" ] || die "no $b/data"
    stop_waydroid
    local keep
    keep=$DATA.aosp17-$(date +%Y%m%d-%H%M)
    echo "Moving the current userdata to $keep and restoring $b/data ..."
    $SUDO mv "$DATA" "$keep"
    $SUDO rsync -aHAX --exclude=/media "$b/data/" "$DATA/"
    # media/ is shared with the backup (hard links): move it back from the current data.
    $SUDO mv "$keep/media" "$DATA/media"
    echo "Restored. The AOSP 17 userdata (without media/) is kept in $keep."
}

install_aosp17() {
    local dir=$1
    [ -f "$dir/system.img" ] && [ -f "$dir/vendor.img" ] || die "no system.img/vendor.img in $dir"
    ls -d "$BACKUPS"/lineage20-* >/dev/null 2>&1 || die "run '$0 backup' first"
    stop_waydroid
    $SUDO mkdir -p "$WD/images-aosp17"
    $SUDO cp "$dir/system.img" "$dir/vendor.img" "$WD/images-aosp17/"
    # The PC's vendor image has its own NVIDIA driver; the LineageOS 20 NVIDIA overlay
    # (tools/waydroid/nvidia-install.sh) stays with that image.
    if [ "$(active)" = lineage20 ] && [ ! -d "$WD/overlay.aosp17" ]; then
        $SUDO mkdir -p "$WD/overlay.aosp17"
    fi
    if [ ! -f "$WD/properties.aosp17" ]; then
        cfg_props save lineage20
        # Keep the user's own settings (model spoof, fingerprint sensor, ...); drop what
        # belonged to the LineageOS 20 image (libhoudini, its gralloc, ANGLE workarounds), and
        # set the AOSP 17 vendor's graphics: Waydroid's own guesses (gralloc=gbm, egl=mesa)
        # end up in waydroid.prop and would override the vendor's properties.
        $SUDO sed -E '/^(ro\.dalvik\.vm\.native\.bridge|ro\.enable\.native\.bridge\.exec|ro\.dalvik\.vm\.isa\.arm(64)?|ro\.product\.cpu\.abilist(32|64)?|ro\.hardware\.(gralloc|egl|vulkan)|gralloc\.gbm\.[a-z]*|debug\.angle\.[a-z_]*|ro\.zygote\.disable_gl_preload|debug\.nvvk\.pipeline_cache_max|ro\.opengles\.version|debug\.hwui\.renderer) =/d' \
            "$WD/properties.lineage20" | $SUDO tee "$WD/properties.aosp17" >/dev/null
        # No boot-control HAL in the container (no misc partition: it aborts, and vold's
        # checkpoint step waits for it forever, which stalls post-fs-data), and none of the
        # Cuttlefish HALs that talk to a host over /dev/hvc* (oemlock, UWB, Thread): they abort,
        # and system_server waits for the declared ones. No frp partition either: an empty
        # ro.frp.pst keeps PersistentDataBlockService off. Waydroid sets
        # debug.stagefright.ccodec=0 when the host has no gralloc HAL, for images with OMX
        # codecs; this one has only Codec2, so 4 (the default) keeps the codec list.
        # The PC images are configured by androidboot.* (ro.boot.*) like Cuttlefish; in
        # Waydroid they come from waydroid.prop. ro.boot.android_pc.waydroid=1 turns on the
        # container pieces (aosp-device/waydroid/init/init.waydroid.vendor.rc).
        printf '%s\n' \
            'ro.boot.android_pc.waydroid = 1' \
            'ro.boot.vendor.apex.com.android.hardware.graphics.composer = none' \
            'ro.boot.vendor.apex.com.android.hardware.keymint = com.android.hardware.keymint.rust_nonsecure' \
            'ro.boot.vendor.apex.com.android.hardware.gatekeeper = com.android.hardware.gatekeeper.nonsecure' \
            'ro.boot.vendor.apex.com.android.hardware.strongbox = none' \
            'ro.boot.vendor.apex.com.android.hardware.secure_element = com.android.hardware.secure_element' \
            'ro.boot.vendor.apex.com.google.emulated.camera.provider.hal = none' \
            'ro.boot.vendor.apex.com.android.hardware.boot = none' \
            'ro.boot.vendor.apex.com.google.cf.oemlock = none' \
            'ro.boot.vendor.apex.com.android.hardware.uwb = none' \
            'ro.boot.vendor.apex.com.android.hardware.threadnetwork = none' \
            'ro.hardware.hwcomposer = waydroid' \
            'ro.hardware.gralloc = minigbm' \
            'ro.hardware.egl = angle' \
            'ro.hardware.vulkan = nvidia' \
            'ro.hardware.camera = ' \
            'ro.frp.pst = ' \
            'debug.stagefright.ccodec = 4' \
            'ro.opengles.version = 196609' \
            'debug.hwui.renderer = skiavk' \
            'ro.zygote.disable_gl_preload = true' \
            | $SUDO tee -a "$WD/properties.aosp17" >/dev/null
    fi
    use aosp17
}

status() {
    echo "active: $(active)"
    grep -E '^(images_path|binder_protocol|service_manager_protocol)' "$WD/waydroid.cfg"
    ls -d "$WD"/overlay* "$WD"/images* "$WD"/properties.* 2>/dev/null
    ls -d "$BACKUPS"/* 2>/dev/null || true
}

case ${1:-} in
    backup) backup ;;
    install) install_aosp17 "${2:?DIR}" ;;
    use) use "${2:?aosp17|lineage20}" ;;
    restore) restore "${2:?BACKUP_DIR}" ;;
    status) status ;;
    *) sed -n '2,15p' "$0"; exit 1 ;;
esac
