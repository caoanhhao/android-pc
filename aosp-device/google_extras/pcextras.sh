#!/vendor/bin/sh
# Google extras (docs/google-extras.md, init.pcextras.rc): Play Store and libndk_translation,
# which the image may not contain, added by the user with tools/add-google-extras.sh.
#  1. New extras on the stick's ESP (android/extras, with a SHA256SUMS list): copied to
#     /data/vendor/pc-extras and checked, once per new set. /data keeps them through OS updates,
#     which rewrite the ESP.
#  2. Every boot: each file in /data/vendor/pc-extras/{system,product} is bind-mounted over the
#     image's placeholder at the same path (Play Store's directory as a whole).
src=/data/vendor/pc-extras
esp=/dev/block/by-name/esp
mnt=/mnt/vendor/pcextras
log() { echo "pcextras: $*" > /dev/kmsg; }

if [ -b $esp ] && mkdir -p $mnt && mount -t vfat -o ro,noatime $esp $mnt 2>/dev/null; then
    new=$mnt/android/extras
    if [ -f $new/SHA256SUMS ] && ! cmp -s $new/SHA256SUMS $src/SHA256SUMS; then
        rm -rf $src.new
        if mkdir -p $src.new && cp -R $new/. $src.new/ && (cd $src.new && sha256sum -c -s SHA256SUMS); then
            rm -rf $src && mv $src.new $src && log "installed $(cat $src/VERSION 2>/dev/null)"
        else
            rm -rf $src.new
            log "copying the extras from the ESP failed, kept the old ones"
        fi
    fi
    umount $mnt
fi
[ -f $src/SHA256SUMS ] || exit 0

# Owned by root, readable by all, labelled as the files they stand in for.
chown -R root:root $src
chmod -R 755 $src
find $src -type f -exec chmod 644 {} +
chmod 755 $src/system/bin/*
chcon -R u:object_r:system_file:s0 $src
chcon u:object_r:system_lib_file:s0 $src/system/lib64/*

n=0
for f in $src/system/lib64/* $src/system/bin/* $src/product/etc/permissions/* \
         $src/product/etc/sysconfig/* $src/product/priv-app/*; do
    [ -e "$f" ] || continue
    t=/${f#$src/}
    if [ -e "$t" ] && mount -o bind "$f" "$t"; then
        n=$((n + 1))
    else
        log "no placeholder for $t, not mounted"
    fi
done
log "mounted $n files from $src ($(cat $src/VERSION 2>/dev/null))"
