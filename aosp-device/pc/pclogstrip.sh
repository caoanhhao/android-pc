#!/vendor/bin/sh
# Phase L1 (docs/nvidia-proprietary.md): the on-screen log strip of the L1 test entries
# (androidboot.android_pc.logstrip=1). Follows the kernel log, keeps the driver's and the tests'
# lines (NVIDIA, nvidia-drm, GSP, Xid, kCFI, pcnvidia, pcnvtest, oopses) in
# /data/local/tmp/logstrip.txt, and starts PcLogStrip, which shows the file's tail on the right
# of the screen.
export PATH=/system/bin:/vendor/bin:$PATH
f=/data/local/tmp/logstrip.txt
: > $f
chmod 0644 $f
appops set com.androidpc.logstrip SYSTEM_ALERT_WINDOW allow
am start-foreground-service -n com.androidpc.logstrip/.LogStripService
dmesg -w | grep --line-buffered -iE 'nvrm|nvidia|nvkms|gsp|xid|cfi failure|pcnv|oops|bug:|panic|drm.*(error|fail)' >> $f
