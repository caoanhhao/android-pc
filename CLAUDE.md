# android-pc

See `PLAN.md` for the overall plan (Rust kernel with Linux/Android ABI on x86_64).

## Build rules (the machine hangs otherwise)

- Builds run **one at a time**, never in parallel across sessions. Before starting
  one, check nothing else is building (`pgrep -af 'soong_ui|ninja|cargo|rustc|gradle'`).
- Every heavy build goes through `tools/limited-build.sh <cmd>`: max **15 CPU
  cores**, **25 GB RAM**, stops when `/` has under **10 GB** free. Pass `-j15`
  (or less) to ninja/make/m; never `-j` with no number.
- Split builds into the smallest targets possible (e.g. AOSP: individual modules
  or one image at a time — `m -j15 <module>`, then `systemimage`, `vendorimage`,
  ... — not a full `m`), checking free disk between steps.
