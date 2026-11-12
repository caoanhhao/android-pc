# One Android VM at a time on this host: a second Cuttlefish/QEMU instance hangs the
# machine. Every VM launcher sources this file before starting its VM:
#
#   . "$ROOT/tools/vm-wait.sh"
#
# It takes an exclusive lock (kept on fd 9, so a VM exec'd by the launcher keeps holding
# it) and then waits until no Cuttlefish/QEMU/crosvm process is left, printing who it is
# waiting for. VM_WAIT_TIMEOUT (seconds, default 0 = forever) gives up with status 1.
_vm_names='qemu-system-x86|crosvm|run_cvd|launch_cvd|cvd_internal_st'
_vm_lock=${XDG_RUNTIME_DIR:-/tmp}/android-pc-vm.lock
exec 9>"$_vm_lock"
if ! flock -n 9; then
    echo "vm-wait: another VM launcher holds $_vm_lock, waiting" >&2
    flock 9
fi
_vm_waited=0
while pgrep -x -- "$_vm_names" >/dev/null; do
    if (( _vm_waited == 0 )); then
        echo "vm-wait: a VM is already running, waiting for it to stop:" >&2
        pgrep -a -x -- "$_vm_names" | cut -c1-160 >&2
    fi
    if (( ${VM_WAIT_TIMEOUT:-0} > 0 && _vm_waited >= VM_WAIT_TIMEOUT )); then
        echo "vm-wait: still running after ${VM_WAIT_TIMEOUT}s, giving up" >&2
        return 1 2>/dev/null || exit 1
    fi
    sleep 10
    _vm_waited=$(( _vm_waited + 10 ))
done
unset _vm_names _vm_lock _vm_waited
