# android-pc

See `PLAN.md` for the overall plan (AOSP 17 on a real x86_64 PC with Rust HALs; the
earlier Rust kernel lives on the `aosp-16` branch).

## Build rules (the machine hangs otherwise)

- Builds run **one at a time**, never in parallel across sessions. Before starting
  one, check nothing else is building (`pgrep -af 'soong_ui|ninja|cargo|rustc|gradle'`).
- Every heavy build goes through `tools/limited-build.sh <cmd>`: it pins the build to the
  **8 E-cores (CPUs 8-15)** with `taskset`, caps it at **20 GB RAM**, nice 10, low I/O
  weight, and stops when `/` has under **10 GB** free. Pass `-j8` (or less) to
  ninja/make/m; never `-j` with no number. Do not move builds onto the P-cores (0-7):
  the i7-13700K is degraded and P-core load froze the host and oopsed its kernel
  (docs/aosp-build.md, "Flaky host"). systemd's `AllowedCPUs` does nothing here (no
  cpuset controller for the user), so always pin with `taskset`.
- AOSP builds lower Soong's memory: `SOONG_GOMEMLIMIT=14GiB SOONG_INCREMENTAL_ANALYSIS=false`
  (the defaults in `tools/aosp17-build.sh`; set them yourself for a bare `m`). With 18GiB
  and incremental analysis the Soong analysis gets OOM-killed under the 20 GB cap.
- Split builds into the smallest targets possible (e.g. AOSP: individual modules
  or one image at a time — `m -j12 <module>`, then `systemimage`, `vendorimage`,
  ... — not a full `m`), checking free disk between steps.

## VM rule (Cuttlefish/QEMU hang the machine)

- **Only one VM at a time** on this host — one Cuttlefish instance *or* one QEMU,
  across all sessions. A session that needs a VM while another one runs **waits**
  for it to stop; never start a second one, never stop another session's VM.
- Start VMs only through `aosp/cvd-launch.sh` or `tools/run-qemu-aosp17-usb.sh`;
  both source `tools/vm-wait.sh`, which takes a lock and waits until no
  `qemu-system`/`crosvm`/`run_cvd` process is left. A new launcher must source it too.
- VMs run pinned to E-cores 12-15 with a memory cap (both launchers do it; keep it in
  any new launcher). Use at most 8 vCPUs and 4 GB guest RAM. More than 4 vCPUs need all
  8 E-cores (`CPUS=8 VM_CPUS=8-15`), so no build may run at the same time.
- Stop your VM as soon as the test is done (`cd aosp/cvd-home && HOME=$PWD ./bin/stop_cvd`,
  or quit QEMU), so the next session can go.

## Waydroid rule (an AOSP 17 container froze the host)

- **Never start Waydroid** (`waydroid session start`, `show-full-ui`, the container
  service, `waydroid init`) unless the user explicitly allows it for that run.
  `waydroid-container.service` stays disabled.
- Why: our AOSP 17 image failed early in the container, rebooted, and init's shutdown
  path ran `EXT4_IOC_SHUTDOWN` on `/data`, a bind mount of the host's root filesystem
  (the container keeps `sys_admin`), which shut down the host's `/` (2026-11-18, twice).
  Any image for Waydroid must first patch init/vold so the container never issues
  filesystem-shutdown ioctls or touches host mounts (docs/waydroid-aosp17.md).

## USB stick rule (keep the user's data)

- The stick is updated **only** with `tools/update-usb-aosp17.sh NEW.img /dev/sdX`: it
  rewrites the OS partitions (esp, vbmeta*, boot, init_boot, vendor_boot, super) and
  leaves `userdata`, `metadata`, `misc`, `frp`, `pclog` and the partition table alone.
- Run it **without `sudo`**: it asks for the password itself and runs only the commands on
  the stick as root. When a new stick image is ready, the session runs the update itself
  (don't hand the user the command): check with `lsblk -o NAME,SIZE,MODEL,TRAN` that
  `/dev/sdX` is the SanDisk USB stick, then type
  `tools/update-usb-aosp17.sh --yes NEW.img /dev/sdX` into the Terminal panel
  (`run_in_terminal`, not Bash: sudo needs a terminal); the user types the password there.
  Read the tab to the end and report `Read back and compared: OK` or the error.
- Never hand the user a whole-image `dd` (or anything that wipes `userdata`) for the
  stick. If an update cannot be done in place (layout changed, AVB key changed, security
  patch level would go back), stop and tell the user why before suggesting a full reflash.
- Every new build must get a fresh build number and must not lower the security patch
  level or the AVB rollback index, so the update script accepts it.
