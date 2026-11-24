#!/vendor/bin/sh
# Lays out the tree gts7l-sensord serves the sensor DSP (SLPI) over FastRPC, under the paths
# the DSP asks for (docs/gts7l.md §19). sensord confines every path to this root, so these are
# copies, not links:
#
#   vendor/etc/sensors/         the stock registry config, from gts7l_fw (tools/gts7l-firmware.sh)
#   mnt/vendor/persist/sensors/ the registry the DSP writes; seeded once from the tablet's
#                               persist partition, without sns_reg_version so the DSP rebuilds it
#                               from the config above
#   sys/..., props              what the DSP reads to pick the board's configuration
#
# Run by init (gts7l-sensors.rc) once /data and gts7l_fw are mounted.
set -e
ROOT=/data/vendor/sensors/root
CONFIG=/mnt/vendor/firmware/sensors
PERSIST=/mnt/vendor/persist/sensors

mkdir -p $ROOT/vendor/etc $ROOT/mnt/vendor/persist/sensors $ROOT/sys/devices/soc0 \
    $ROOT/sys/class/sensors/ssc_core
rm -rf $ROOT/vendor/etc/sensors
cp -r $CONFIG $ROOT/vendor/etc/sensors

registry=$ROOT/mnt/vendor/persist/sensors/registry/registry
if [ ! -d $registry ] || [ -z "$(ls $registry)" ]; then
    cp -r $PERSIST/. $ROOT/mnt/vendor/persist/sensors/
    rm -f $ROOT/mnt/vendor/persist/sensors/registry/sns_reg_version
fi

# The board revision selects the sensor configuration. Our kernel forces its own command line,
# so it is read from the one ABL passed in the DT; 7 is what this tablet's ABL passes.
revision=$(tr '\0 ' '\n\n' < /proc/device-tree/chosen/bootargs | sed -n 's/^androidboot\.revision=//p')
revision=${revision:-7}
# SM8250 (soc_id 356) v2.1 on Samsung's MTP-derived board, as the stock DT and socinfo say;
# the mainline kernel has no socinfo node for these.
echo 356 > $ROOT/sys/devices/soc0/soc_id
echo 2.1 > $ROOT/sys/devices/soc0/revision
echo MTP > $ROOT/sys/devices/soc0/hw_platform
echo strange > $ROOT/sys/devices/soc0/platform_subtype
echo 2 > $ROOT/sys/devices/soc0/platform_subtype_id
echo 65536 > $ROOT/sys/devices/soc0/platform_version
echo $revision > $ROOT/sys/class/sensors/ssc_core/ssc_hw_rev
echo ro.revision=$revision > $ROOT/props
