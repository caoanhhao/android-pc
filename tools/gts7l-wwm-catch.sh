#!/system/bin/sh
# Where Winds Meet crash catcher for the gts7l tablet (docs/gts7l.md, section 26).
#
# The game's breakpad handler (libAndroidCrashHandler.so) catches SIGSEGV, logs
# "JNI [NCCrashHandler] start 1 sig=11", forks a dumper that deadlocks and is killed by a
# 15 s watchdog, so there is no tombstone. This waits for that log line, SIGSTOPs the game
# (it stays stopped: kill -9 it when done) and saves the crashing thread's syscall
# registers, its alternate signal stack (which holds the rt_sigframe with the faulting
# registers), the stack the fault happened on, the maps, logcat and dmesg into
# /data/local/tmp/gcatch.
#
# Run as root, quoted so adb keeps the quotes (adb shell joins its arguments: with
# `adb shell su 0 sh -c '...'` only the first word runs as root):
#   adb push tools/gts7l-wwm-catch.sh /data/local/tmp/gcatch.sh
#   adb shell "su 0 sh -c 'nohup sh /data/local/tmp/gcatch.sh > /data/local/tmp/gcatch.log 2>&1 &'"
O=/data/local/tmp/gcatch; rm -rf $O; mkdir -p $O
l=$(logcat -T 1 -m 1 -e 'NCCrashHandler\] start .*sig=' | grep sig=)
set -- $l; P=$3; T=$4
kill -STOP $P
echo "$l" > $O/line
echo $P > $O/pid
# SIGSTOP is asynchronous: wait until the thread has stopped, or syscall reads "running".
i=0; while grep -q running /proc/$P/task/$T/syscall && [ $i -lt 50 ]; do sleep 0.1; i=$((i+1)); done
cat /proc/$P/task/$T/syscall > $O/syscall
cat /proc/$P/maps > $O/maps
for t in /proc/$P/task/*; do echo "$(basename $t) $(cat $t/comm) $(cat $t/syscall)"; done > $O/tasks
set -- $(cat $O/syscall); sp=$8
# mksh arithmetic is 32-bit: keep addresses as hex strings and page-align with sed.
b=$(echo $sp | sed 's/...$/000/')
echo $b > $O/stack.base
dd if=/proc/$P/mem of=$O/stack.bin bs=4096 iflag=skip_bytes skip=$b count=16 2>$O/dd.err
# rt_sigframe: FPSIMD magic 0x46508001 (16-byte aligned); the interrupted sp is 32 bytes
# before it, the faulting pc 24 bytes before it.
osp=$(od -A d -t x8 -v $O/stack.bin | awk '{if (substr($2,9)=="46508001") {print p2; exit} p2=p1; p1=$2}')
echo "0x$osp" > $O/orig.sp
ob=$(echo "0x$osp" | sed 's/...$/000/')
echo $ob > $O/ostack.base
dd if=/proc/$P/mem of=$O/ostack.bin bs=4096 iflag=skip_bytes skip=$ob count=64 2>$O/odd.err
logcat -d -b all > $O/logcat.txt
dmesg > $O/dmesg.txt
chmod 666 $O/*
echo done > $O/status
