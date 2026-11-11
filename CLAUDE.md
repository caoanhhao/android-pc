# android-pc

See `PLAN.md` for the overall plan (AOSP 17 on a real x86_64 PC with Rust HALs; the
earlier Rust kernel lives on the `aosp-16` branch).

## Build rules (the machine hangs otherwise)

- Builds run **one at a time**, never in parallel across sessions. Before starting
  one, check nothing else is building (`pgrep -af 'soong_ui|ninja|cargo|rustc|gradle'`).
- Every heavy build goes through `tools/limited-build.sh <cmd>`: max **12 CPU
  cores**, **25 GB RAM**, stops when `/` has under **10 GB** free. Pass `-j12`
  (or less) to ninja/make/m; never `-j` with no number.
- Split builds into the smallest targets possible (e.g. AOSP: individual modules
  or one image at a time — `m -j12 <module>`, then `systemimage`, `vendorimage`,
  ... — not a full `m`), checking free disk between steps.
