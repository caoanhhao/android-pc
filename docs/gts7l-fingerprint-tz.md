# gts7l fingerprint: the stock HAL's TrustZone protocol

Step 2 of the plan in `docs/gts7l.md` §25 ("The real sensor: plan"): what Samsung's stock
fingerprint HAL sends to its TrustZone apps (TAs), detailed enough to write a Rust AIDL
`IFingerprint` HAL against the same TAs. Pure reverse engineering of the stock firmware
`T875XXS8DXH1`; no tablet, no builds.

Sources (all from `gts7l-stock/`, read with `llvm-objdump`/`readelf`/`strings`, never run):

| File | What it is |
|---|---|
| `ap/super/vendor.img`: `bin/hw/vendor.samsung.hardware.biometrics.fingerprint@3.0-service` | HIDL service; `hw_get_module("fingerprint")` |
| `lib64/hw/fingerprint.default.so` | legacy HAL shim, forwards to `ss_fingerprint_*` in `libbauthserver.so` |
| `lib64/libbauthserver.so` (494 KB, C++ with symbols) | the real HAL: `FPBAuthService`, sensor control, storage, `gf*` Goodix device code |
| `lib64/libbauthtzcommon.so` (40 KB) | `BAuth_*`: the only code that talks to `QSEECom_*` |
| `lib64/libQSEEComAPI.so` | Qualcomm's QSEECom client (`/dev/qseecom` + ION) |
| `apex/com.samsung.android.biometrics.fingerprint.signed.apex` | **empty** on this firmware (payload holds only `apex_manifest.pb`, no `etc/ta/fpta`) |
| `blcp/NON-HLOS.bin`: `image/securefp.*`, `authhat.*`, `skeymast.*`, `keymaster*`, `fingerpr.*` | the TAs (FAT image; `mcopy -i NON-HLOS.bin ::/image/X`) |

Confidence marks used below: **(verified)** = read directly from the instructions or the
TA's own strings; **(inferred)** = follows from the code but a field's meaning is a guess;
**(open)** = not resolved, see §10. Addresses are in `libbauthtzcommon.so` (`tzc`) or
`libbauthserver.so` (`bas`), for whoever continues the work.

## 1. Summary

- The GW36T1 is served by TA **`securefp`** (`//SEC_TA_VERSION:7.0.0.1//`, its strings name
  `GW36T1/T2/T3` and Goodix's library). **`fingerpr` is not used** on this device: it is the TA
  for other sensor families. (verified)
- `securefp` uses no secure file system and raises no listener requests (no `qsee_sfs*`,
  no listener strings): templates are returned to the HAL encrypted and the HAL stores them
  in `/data/vendor/biometrics`. `qseecomd` is not needed for fingerprint. (verified from the
  TA's import strings; runtime confirmation is still open)
- All FP commands share one framing: a request and a response in one shared buffer,
  `u32 cmd` first, TA status at `rsp+4`. 16 command IDs (§4), one of which (`0x0C`) is a
  generic "control op" carrying ~40 sub-opcodes (§5).
- The TA drives the capture loop: every enroll/identify response carries an **fpop** that
  tells the HAL what to do next (wait for the IRQ, reset the sensor, report finger down/up,
  call again) (§6).
- Enroll requires a gatekeeper HW auth token whose HMAC **the TA itself verifies**
  (`tl_hat_op_stub` → `IntegrityCheckAuthToken`). The key comes from a keymaster TA through
  `authhat` (§7). With AOSP's software gatekeeper this check fails; this is the main risk
  for step 3 (§10).
- Normal-world sensor control is six ioctls on `/dev/goodix_fp` plus a netlink socket that
  carries the IRQ (§8).

## 2. TAs and how they start

`QSEECom_start_app(handle**, path, name, sb_size)`; the library appends `.mdt`/`.bXX`.
Paths are as the stock userspace passes them (`/vendor/firmware_mnt/image`); on the tablet
the files are in `/mnt/vendor/apnhlos/image/` too (same NON-HLOS content).

| TA | Who starts it | When | Shared buffer (`sb_size`) |
|---|---|---|---|
| `securefp` | `BAuth_SessionOpen(tl_type)` (`tzc 0x3568`) | session open (service start, every `request()`/enroll/identify if closed) | `0x1F4140` (2 048 320). Commands touch up to `0x1F4180` (control op memsets that much), so allocate ≥ `0x1F5000` |
| APEX `fpta` (named `securefp`) | `QSEECom_start_app_V2(h, "securefp", buf, len, 0x1F4140)` | only if `/apex/com.samsung.android.biometrics.fingerprint/etc/ta/fpta` exists and its `SEC_TA_VERSION` is higher than the preloaded one | same |
| `dualfp` | same function | `tl_type` 3, or 8 without the "tsf" flag | same |
| `authhat` | `BAuth_GetK_From_KM` | only while provisioning the HAT HMAC key (§7) | `0x800` |
| `skeymast` (Samsung keymaster) | `BAuth_GetK_From_KM` | key provisioning when `ro.hardware.keystore` = `mdfpp` | `0x140C0` |
| `keymaster`, then `keymaster64` | `BAuth_GetK_From_KM` | key provisioning otherwise | `0x40000` |
| `fingerpr`, `skm` | not referenced by `libbauthtzcommon`/`libbauthserver` for this sensor | never | — |

**TA name selection** (`tzc 0x8428`): `tl_type` 1..9 is the `SessionOpen` argument.
Types {1, 5, 6, 7, 9} → `securefp`; {3} → `dualfp`; 8 → `securefp` if the "tsf" flag
(control op `0x5C`) was set, else `dualfp`; anything else fails ("unknown ta name").
Types 7..9 also set `ta_switch_flag = 2` (else 1), which turns an `EINVAL` from
`QSEECom_send_cmd` into status `0x43` ("session not open") instead of an error.

**`tl_type` for GW36T1** (`bas FPBAuthService::check_gpio_snsr_type`, `0x4CE9C`): the HAL
reads up to 10 bytes of `/sys/class/fingerprint/fingerprint/name` and matches the first
5 bytes. `"GW36T"` → **tl_type 6, sensor id 18**. (verified) The full table:

| name prefix | tl_type | sensor id | | name prefix | tl_type | sensor id |
|---|---|---|---|---|---|---|
| `VIPER` | 1 | 2 | | `NAMSAN` | 5 | 6 |
| `ET320` | 3 | 3 | | `GW32J` | 6 | 7 |
| `ET510` / `ET516` | 3 | 4 / 5 | | `QBT2000` | 7 | 10 |
| `ET520` / `ET523` / `ET528` | 3 | 8 / 9 / 20 | | `GW36C` / `GW36H` | 6 | 11 / 12 |
| `EC617` | 3 | 22 | | `ET603` | 3 | 13 |
| `ET711` / `ET713` / `ET715` | 8 | 14 / 15 / 16 | | `GW95` (4 bytes) | 9 | 17 |
| `QBT2608` | 7 | 19 | | **`GW36T`** | **6** | **18** |
| `EL721` | 8 | 21 | | | | |

So our kernel driver must expose `/sys/class/fingerprint/fingerprint/name` = `GW36T1` if
anything reads it; a Rust HAL can simply hard-code tl_type 6 and TA `securefp`.

**Bandwidth.** Before the first command after a session open, `QSEECom_set_bandwidth(h, 1)`
("high"); `QSEECom_set_bandwidth(h, 0)` on session close and after control op `0x25`
(template pre-decrypt). On mainline this maps to voting the crypto engine clocks; it is a
performance hint, not a protocol step.

**Shutdown.** `QSEECom_shutdown_app` retried up to 30 times, 100 ms apart.

**TA version.** `BAuth_Get_Ta_Version` does not call the TA: it scans the TA files for the
string `//SEC_TA_VERSION:x.y.z.w//` (`securefp`: `7.0.0.1`).

## 3. Framing of every FP command

```
shared buffer (handle->ion_sbuffer)
 +0            request  (req_len bytes, 64-byte multiple)
 +req_len      response (rsp_len bytes, 64-byte multiple)
QSEECom_send_cmd(h, sb, req_len, sb + req_len, rsp_len)
```

- The HAL `memset`s `req_len + rsp_len` (rounded to 64) to zero before each command.
- Request: `u32 cmd` at `+0`; most commands put an in/out "opcode" word at `+8` (some at `+4`).
- Response: `u32 opcode_out` at `+0` (the **fpop**, §6), `u32 status` at `+4` (0 = OK; the
  `BAuth_*` function returns it), command data after that.
- `BAuth_*` return codes: 0 OK, `0x1D` (29) generic failure / no handle, `0x43` (67) session
  not open, `0x35` (53) "passed session is not closed", `0x3F` (63) bad input length, else
  the TA status or the `QSEECom_send_cmd` return value.
- All integers little-endian. Strings are NUL-padded fixed fields.

## 4. FP command IDs (`securefp`)

Argument names are the `BAuth_*` C signatures (`aN` = Nth argument); offsets are from the
start of the request (`req+`) or response (`rsp+`). TA-side names come from `securefp`'s
log strings.

| cmd | `BAuth_*` (`tzc` addr) | TA name | req_len | rsp_len |
|---|---|---|---|---|
| `0x01` | `BAuth_Prepare` (`0x3AB8`) | `tl_prepare_stub` | `0xA040` | `0xA040` |
| `0x02` | `BAuth_Enroll_Init` (`0x3DF8`) | `TZ_CMD_ENROLL_INIT` | `0x180` | `0x40` |
| `0x03` | `BAuth_Enroll_Do` (`0x4188`) | `TZ_CMD_ENROLL_DO` | `0x40` | `0x47440` |
| `0x04` | `BAuth_Enroll_Final` (`0x44A0`) | — | `0x40` | `0xA040` |
| `0x05` | `BAuth_Identify_Init` (`0x4788`) | `TZ_CMD_IDENTIFY_INIT` | `0x3D9C0` | `0x1C0` |
| `0x06` | `BAuth_Identify_Do` (`0x4C4C`) | `TZ_CMD_IDENTIFY_DO` | `0x40` | `0x474C0` |
| `0x07` | `BAuth_Identify_Final` (`0x4FD4`) | — | `0x40` | `0xA040` |
| `0x08` | `BAuth_Set_Password` (`0x52BC`) | — | `0x540` | `0x440` |
| `0x09` | `BAuth_Verify_Password` (`0x55CC`) | — | `0x940` | `0x40` |
| `0x0A` | `BAuth_Cancel` (`0x58D4`) | — | `0x40` | `0x40` |
| `0x0B` | — (unused) | | | |
| `0x0C` | `BAuth_Control_OP` (`0x5B4C`) | `do_extra_control` | `0xFA140` | `0xFA040` |
| `0x0D` | `BAuth_Hat_OP` (`0x6438`) | `tl_hat_op_stub` | `0x480` | `0x440` |
| `0x0E` | `BAuth_Template_Migration` (`0x6788`) | — | `0x3D580` | `0x3D440` |
| `0x0F` | `BAuth_Decap_Key` (`0x3250`) | `TZ_CMD_DECAP_KEY` | `0x440` | `0xA040` |
| `0x10` | `BAuth_Type_Check` (`0x61C0`) | — | `0x40` | `0x40` |

`BAuth_Open`/`BAuth_Close` only log (no TA call). `BAuth_SessionClose` sets bandwidth low,
shuts the TA down and frees the handle (refuses while an enroll session flag is set).

### 4.1 `0x01` Prepare — `(u32 *op_flag, u8 *in, u32 in_len, u8 *out, u32 *out_len, u32 *extra)`

| req | | rsp | |
|---|---|---|---|
| `+8` | `*op_flag` (`BAUTH_OP_CODE_FIRST_CALL` on the first call, per the TA's checks) | `+0` | → `*op_flag` (fpop) |
| `+0xC` | in data (≤ `0xA000`) | `+4` | status |
| `+0xA00C` | `in_len` | `+8` | → `*extra` |
| | | `+0xC` | out data, length at `rsp+0xA00C` (≤ `0xA000`) → `out`, `*out_len` |

Used by `FPBAuthService::common_prepare` (before every enroll/identify): first call with no
data, then again with the calibration/metadata the TA asked for; the TA refuses
`in_data_len > 0` on the first call. The out data is a blob the HAL stores as metadata
(`check_file_size`, then `storeMetaData`). (inferred)

### 4.2 `0x02` Enroll_Init — `(u32 *op, char *user_id, u32 id_len, u32 bio_idx, u32 *out, u32 type, char *storepath)`

req: `+8` `*op`; `+0xC` user id (≤ 256 bytes); `+0x10C` `bio_idx` (1..4, the finger slot,
from `BAuthUserInfo::get_req_bioindex`); `+0x110` `type` (the HAL passes 7); `+0x114`
storepath (≤ 100 chars). rsp: `+0` → `*op`, `+4` status, `+8` → `*out`.

### 4.3 `0x03` Enroll_Do — `(u32 *op, u8 *out, u32 *out_len, u32 prog[3], u32 *a4, u32 *a5)`

req: `+8` `*op` (HAL sets 1). rsp: `+0` → `*op` (fpop), `+4` status, `+8` → `*a5`,
`+0xC` → `*a4`, `+0x10` → `prog[0]`, `+0x14` → `prog[2]`, `+0x18` → `prog[1]`,
`+0x1C` out data (image/debug, ≤ `0x3D400`), its length at `rsp+0x3D41C`.
The TA logs `enrolled_count`, `total_count`, `percentage`, `quality` and `opcode` for this
command, so `prog[]`/`a4`/`a5` carry those (exact order **open**; the HAL fills
`BAuthOpResult` keys `KEY_ENROLL_PERCENTAGE`, `KEY_SUCCESS_ENROLL_CNT`,
`KEY_TOTAL_ENROLL_CNT`, `KEY_IMAGE_QUALITY` from them). Return `0x30` and `0x46` are special
cases in the HAL (`bas 0x576D0`): treat as "bad image, retry" until resolved.

### 4.4 `0x04` Enroll_Final / `0x07` Identify_Final — `(u32 *op, struct out *o, u32 *a2)`

Same layout. req: `+8` `*op`. rsp: `+0` → `*op`, `+4` status, `+8` → `o[1]`, `+0xC` →
`o[0]`, `+0x10` → `*a2`, `+0x14` data (≤ `0xA000`) → `o + 8`, length at `rsp+0xA014` →
`o[0xA008/4]`. For enroll the data is the **encrypted template** the HAL writes to
`.../fp/User_<n>/<user>_<bioidx>tmpl.dat` (inferred from `storeTemplate` right after).

### 4.5 `0x05` Identify_Init — 11 arguments

`(u32 *op, char *user_id, u32 id_len, u8 *templates, u32 tmpl_len, u8 *aux, u32 aux_len,
u32 *chunk, u8 *out, u32 *out_len, hw_auth_token_t *hat_template)`

| req | | rsp (`+0x3D9C0`) | |
|---|---|---|---|
| `+4` | `*chunk` (template chunk index, `-1` = no templates) | `+0` | → `*op` |
| `+8` | `*op` | `+4` | status |
| `+0xC` | user id (≤ 256) | `+8` | → `*chunk` |
| `+0x10C` | templates (≤ `0x3D400` per call) | `+0xC` | out data → `out` |
| `+0x3D50C` | `tmpl_len` (0 if > `0x3D400`) | `+0x194` | out length (< `0x190`) → `*out_len` |
| `+0x3D510` | aux data (≤ `0x400`, e.g. FIDO) | | |
| `+0x3D910` | `aux_len` | | |
| `+0x3D914` | **69-byte HAT template** | | |

The HAL concatenates all stored templates of the user (`retrieveAllTemplates`) and sends
them in chunks of `0x3D400` bytes, `chunk` = 0, 1, 2 (≤ 3 calls). **The challenge goes in
the HAT template**: `version = 0`, `challenge = operation_id` at byte 1, `authenticator_type
= 2` (fingerprint) at byte 25, everything else zero (`bas 0x5A4A0`). The TA fills user id,
authenticator id, timestamp and HMAC on a match.

### 4.6 `0x06` Identify_Do — `(u32 *op, u32 res[2], u32 *a2, u8 *out, u32 *out_len, u32 *a5, hw_auth_token_t *hat_out)`

req: `+4` `*a2`, `+8` `*op` (HAL sets 1). rsp: `+0` → `*op` (fpop), `+4` status, `+8` →
`*a5`, `+0xC` → `*a2`, `+0x10` → `res[0]`, `+0x14` → `res[1]`, `+0x38` out data
(≤ `0x3D400`, length at `rsp+0x3D438`), `+0x47440` the **69-byte HW auth token**, copied to
`hat_out` only if the `u32` at `rsp+0x47485` is 1.
TA strings: `matched_score`, `matched_bioidx` (0xFF = none), `quality`, `opcode`. The match
result is therefore `score`/`bio_idx` in `res[]`/`a5` (which is which: **open**). The
finger id reported to Android is the HAL's bio index (1..4), mapped through
`BAuthUserInfo` (§9).

### 4.7 `0x08` Set_Password / `0x09` Verify_Password

Backup-password feature of old Samsung firmware; not needed. Set: req `+8` `*a0`, `+0xC`
user id (≤ 256), `+0x10C` data (≤ `0x400`), `+0x50C` len; rsp `+0` → `*a0`, `+4` status,
`+8` data, `+0x408` len. Verify: req `+8`, `+0xC` user id, `+0x510` data1 (`+0x910` len),
`+0x10C` data2 (`+0x50C` len); rsp `+0`, `+4`.

### 4.8 `0x0A` Cancel — `(u32 *a0)`

req `+4` `*a0`; rsp `+0` → `*a0`, `+4` status. Called on cancel, on errors in the capture
loop, and from the signal handler. The HAL also posts the IRQ semaphore with a cancel flag
(§8) so a blocked wait returns.

### 4.9 `0x0C` Control_OP — `(u32 *opcode, char *user_id, u8 *in, u32 in_len, u8 *out, u32 *out_len)`

| req | | rsp (`+0xFA140`) | |
|---|---|---|---|
| `+4` | op-specific word (see below) | `+0` | → `*opcode` (fpop) |
| `+8` | `*opcode` | `+4` | status |
| `+0xC` | user id (strnlen ≤ 256) | `+8` | value: → `*user_id` (as `u32`) for op `0x3D`, → `*out_len` for op `0x46` |
| `+0x10C` | in data (< `0xFA001`) | `+0xC` | out data → `out` |
| `+0xFA10C` | `in_len` | `+0xFA00C` | out length; preset by the HAL to the capacity `*out_len` |

Op-specific word at `req+4`: op `0x51` → `*out_len`; op `0x52` → `in_len`; op `0x0C` →
`*out_len` and capacity forced to `0x3D400`; op `0x25` → `*out_len` (chunk index) and
capacity forced to 400. For op `0x43` the first byte of `out` is copied to `rsp+0xC`
before the call.

Handled in `libbauthtzcommon` without a TA call: `0x16` (sets the enroll-session flag;
`in_len == 1` → set), `0x1E` while that flag is set (refused), `0x31` with `in_len == 0`
(returns `0x3F`), `0x53`, `0x54`, `0x5B` (no-ops), `0x5C` (stores `in[0]` as the "tsf" flag,
§2), `0x6C` (no-op).

### 4.10 `0x0D` Hat_OP — `(u32 *a0, hw_auth_token_t *hat, u8 *in, u32 in_len, u8 *out, u32 *out_len)`

req: `+4..+0x48` the 69-byte HAT, `+0x49` `*a0` (unaligned `u32`), `+0x4D` in data
(≤ `0x400`), `+0x44D` `in_len`. rsp (`+0x480`): `+0` → `*a0`, `+4` status, `+8` out data,
`+0x408` out length. Enroll calls it with the gatekeeper HAT and nothing else; the TA
checks `hat->version` (0), `hat->challenge != 0`, `challenge == ctx->preEnrollChallenge`,
then `IntegrityCheckAuthToken` (HMAC). See §7.

### 4.11 `0x0E` Template_Migration

`(u32 *a0, char *user_id, u32 id_len, char *storepath, u8 *in, u32 in_len, u8 *out,
u32 *out_len, u32 a8)`: req `+4` storepath (≤ 100), `+0x68` `*a0`, `+0x6C` in (≤ `0x3D400`),
`+0x3D46C` `in_len`, `+0x3D470` `a8`, `+0x3D474` user id (≤ 256); rsp (`+0x3D580`): `+0` →
`*a0`, `+4` status, `+8` out, `+0x3D408` out length. Only from `set_active_group`, to
re-encrypt templates from an older TA format. Not needed for fresh enrollments.

### 4.12 `0x0F` Decap_Key — `(u8 *in, u32 in_len, u8 *out, u32 *out_len)`

req `+4` in (≤ `0x400`), `+0x404` `in_len`. rsp (`+0x440`): `+4` status, `+8` out,
`+0xA008` out length. Feeds the HAT HMAC key, wrapped by `authhat`, into `securefp`; the
output is a blob the HAL caches and replays with control op `0x31` (§7).

### 4.13 `0x10` Type_Check — `(u32 *val)`

req `+4` `*val`. rsp: **`+0` status**, `+4` → `*val` (the only command with this order).
`readSensorType` calls it after a bare session open to confirm the sensor type stored in
`/data/vendor/biometrics/type`.

## 5. Control-op sub-opcodes

Names from the HAL's and TA's strings where they exist; meaning (inferred) otherwise.

| op | Name / use | Called from |
|---|---|---|
| `0x0B` (11) | `BAUTH_OP_CODE_GET_TEMPL_ID` | `getEnrolledTemplateId` |
| `0x0C` (12) | raw capture (out ≤ `0x3D400`) | sensor test |
| `0x16` (22) | `BAUTH_OP_CODE_SET_ENROLL_SESSION` (local) | `setEnrollSession` |
| `0x18` (24) | "request/wait" step inside the enroll loop | `enroll` |
| `0x19` (25) | before `Enroll_Init`, out ≤ 1000 bytes logged as "cbd" | `enroll` (only when `tl_type == 7`) |
| `0x1E` (30) | `BAUTH_OP_CODE_CLEAN_UP`: returns metadata to save | session close, destructor, signal handler |
| `0x25` (37) | template **pre-decrypt**: all templates in `0x3D400` chunks, `req+4` = chunk index | `request()` (session start) |
| `0x2D` (45) | `BAUTH_OP_CODE_SAVE_SECAUTH_ID` | after enroll, `set_active_group` |
| `0x2E` (46) | `BAUTH_OP_CODE_GET_AUTH_ID` → authenticator id, stored as `sec_auth_id.dat` | after enroll, identify, `set_active_group`, `update_sid` |
| `0x2F` (47) | `BAUTH_OP_CODE_SEND_SECAUTH_ID_DATA`: in = `sec_auth_id.dat` | `request()`, session close |
| `0x30` (48) | `BAUTH_OP_CODE_SEND_STOREPATH`: in = 100-byte path | `request()`, remove, session close |
| `0x31` (49) | `BAUTH_OP_CODE_SEND_KEY`: in = cached HAT key blob (§7) | `request()`, enroll, identify, `update_sid` |
| `0x32` (50) | sent twice before enroll/identify when a feature flag is set | enroll, identify |
| `0x34`/`0x35` | `BAUTH_OP_CODE_NAVIGATION_START/WORKING` (+ stop) | gestures, not needed |
| `0x41` (65) | generic follow-up after an fpop | `check_opcode` |
| `0x43` (67) | CBGE update (bezel/background estimation) | `update_cbge` |
| `0x45`/`0x47` | CBGE operations | `new_cbge_mode` |
| `0x4A`/`0x4B` | identify/enroll debug info ("iae", "cbd") | |
| `0x51`/`0x52` | load/update "BDS" calibration blob | `load_bds`, `update_bds` |
| `0x53`/`0x54` | local no-ops (`NOTIFY_REMOVE_FINGER` / `WAIT_INTERRUPT` bookkeeping) | `check_opcode` |
| `0x57` (87) | "tfd" (finger detect) | `check_opcode` |
| `0x58` (88) | prepare HMAC, `in_len` 10 | `common_prepare` |
| `0x59` (89) | `BAUTH_OP_CODE_LATE_TEMPLATE_UPDATE` (template refresh after a match) | identify |
| `0x5C` (92) | "tsf" flag (local) | constructor, `update_matcher` |
| `0x5E`/`0x5F` | optical (GDX-OPT) calibration | not for GW36 |
| `0x61` (97) | sensor device control | `sensor_device_control` |
| `0x64`/`0x65` | "sdv", timestamp ("sfqdbtz") | `common_prepare`, identify |
| `0x6C` (108) | local no-op | identify |
| `0x191`/`0x192` | LCD panel / window type | `common_prepare` (optical only) |
| `0x1F6`/`0x1F7` | Qualcomm ultrasonic calibration | not for GW36 |

Remove (`BAUTH_OP_CODE_REMOVE_TEMPLATE`), enumerate and the pre-enroll challenge
(`tl_generate_challenge_stub` in the TA) are reached through `FPBAuthService::request()` /
`runRequestThread` with an opcode chosen at run time, so the number is not visible at a
call site: **open** (§10). `GET_BIOINFO_LIST`, `GET_ENROLL_SESSION`, `GET_RAW_TEMPLATE_ID`,
`ENABLE_ASP`, `IPC_PROBE` also exist in the TA.

## 6. fpop: what the TA asks the HAL to do

After `Enroll_*`, `Identify_*` and `Prepare`, `rsp+0` is an opcode that
`FPBAuthService::check_opcode` (`bas 0x4E23C`, jump table at `bas 0x22B6E`, 0..0x66) acts on:

| fpop | HAL action |
|---|---|
| 0 | done, nothing to do |
| 3 | `BAUTH_OP_CODE_NOTIFY_REMOVE_FINGER`: enable the IRQ, wait for finger up |
| 4 | `BAUTH_OP_CODE_WAIT_INTERRUPT`: enable the IRQ, **turn the SPI clock vote off**, wait for the IRQ (`timeout` from the caller; -1 = forever), then call the same command again |
| 5 | finger-down notification to the framework (`FINGERPRINT_ACQUIRED`, vendor code 10002), CPU speed-up |
| 7, 8 | reset the sensor (`GF_IOC_RESET`), with CPU speed-up, then call again |
| 9, `0x3E` | CPU speed-up + control op + sensor device control |
| `0x29`, `0x2A` | "device ready" |
| `0x37`..`0x3C`, `0x3F` | misc control ops |
| `0x42`/`0x43`, `0x46`/`0x47` | CBGE load/update |
| `0x57` | finger detect ("tfd") |
| `0x33`, `0x44`, `0x66` | end of session ("cso") |
| others | "Command is invalid" |

`check_opcode` returns 0 to loop (call `Enroll_Do`/`Identify_Do` again), 3 for "finished",
4 for "paused / need loop" (`BAUTH_RET_REQUEST_PAUSE`, `BAUTH_NEED_LOOP`). The capture loop
is therefore: send the command → handle fpop → repeat, with the IRQ wait in between.

## 7. HW auth token and its HMAC key

`hw_auth_token_t` is AOSP's 69-byte packed struct (`HW_AUTH_TOKEN_VERSION` = 0 in
`libbauthtzcommon`): `u8 version; u64 challenge; u64 user_id; u64 authenticator_id;
u32 authenticator_type (big-endian); u64 timestamp (big-endian); u8 hmac[32]`.

**Key provisioning** (`BAuth_GetK_From_KM`, `tzc 0x6C04`, called when control op `0x31`
with the cached blob fails, i.e. on first use):

1. Start `authhat` (sb `0x800`).
2. Keymaster TA:
   - `ro.hardware.keystore` ≠ `mdfpp`: start `keymaster` (or `keymaster64`), sb `0x40000`;
     send `{u32 0x205, u32 2}` (req `0x40`, rsp `0xC0`); `rsp+4` = offset, `rsp+8` = length
     (≤ 256) of the wrapped key inside the response.
   - `mdfpp` (Samsung keystore): start `skeymast`, sb `0x140C0`; request `{0x203, 8, 0xA000}`
     plus the target name `"authhat"`, req `0x14080`, rsp `0x40`.
3. `authhat` cmd 1: req `{u32 1; u8 key[≤1000] at +4; u32 len at +0x3EC}`, req `0x400`,
   rsp `0x40` (status at `rsp+0`).
4. `authhat` cmd 2: req `{u32 2; u32 target}` (`target` 1 = `securefp` types, 2 = `dualfp`),
   req `0x40`, rsp `0x400`; key blob at `rsp+0`, length `rsp+0x3E8`, status `rsp+0x3EC`.
5. `securefp` cmd `0x0F` Decap_Key with that blob; its output is cached by the HAL
   (`FPBAuthService + 0x10E8AC4`, length at `+0x10E8EC4`) and replayed with control op
   `0x31` on every later session.

The TA uses this key both to **verify** the gatekeeper HAT in `Hat_OP` (enroll) and to
**sign** the HAT it returns from `Identify_Do`. Keystore keys bound to biometric auth
verify that HMAC in the keymaster TA, which shares the key.

## 8. Normal world: `/dev/goodix_fp`, IRQ, sysfs

`gf*` functions in `libbauthserver` (`bas 0x706C0`..`0x71740`), source
`FPGDXBAuthSensorControl.cpp`. ioctl magic `'g'`:

| ioctl | number | arg | Use |
|---|---|---|---|
| `GF_IOC_INIT` | `0x80016700` `_IOR('g',0,u8)` | `u8 *route` → netlink protocol number | open the IRQ channel |
| `GF_IOC_EXIT` | `0x6701` | — | close it |
| `GF_IOC_RESET` | `0x6702` | — | pulse reset (gpio127) |
| `GF_IOC_ENABLE_IRQ` | `0x6703` | — | enable gpio63 IRQ |
| `GF_IOC_DISABLE_IRQ` | `0x6704` | — | disable it |
| `GF_IOC_ENABLE_SPI_CLK` | `0x40046705` `_IOW('g',5,u32)` | `u32 *` = 1 | vote the SE7 clock for TrustZone |
| `GF_IOC_DISABLE_SPI_CLK` | `0x6706` | — | drop the vote |
| `GF_IOC_ENABLE_POWER` | `0x6707` | — | LDO on (gpio117) |
| `GF_IOC_DISABLE_POWER` | `0x6708` | — | LDO off |
| `GF_IOC_REMOVE` | `0x670C` | — | |
| (sensor type) | `0x40046712` `_IOW('g',0x12,u32)` | `u32 *` | tells the driver the sensor type |
| `GF_IOC_SPEEDUP` | `0x40046714` | `u32 *` | CPU boost hint |
| `GF_IOC_SET_LOCKSCREEN` | `0x40046715` | `u32 *` | |
| `GF_IOC_GET_ORIENT` | `0x80046716` | `u32 *` | sensor orientation |
| `GF_IOC_MODEL_INFO` | `0x80046717` | 10-byte buffer | |
| `GD_IOC_SET_SPI_STATUS` | `0x8004671A` | `u32 *` | |

Sequences:

- **Power up** (`BAuthDeviceCtrl(1)`): `ENABLE_POWER`, `ENABLE_SPI_CLK(1)`, `GF_IOC_INIT`
  and start the netlink thread. **Power down**: `DISABLE_SPI_CLK`, `DISABLE_POWER`.
- **IRQ**: `socket(AF_NETLINK, SOCK_RAW, route)`, bind to `getpid()`, send one message:
  `nlmsghdr{len=32, pid=getpid()}` + payload `"GF\0"`. The kernel then sends messages whose
  first payload byte is 1 for "IRQ" (0 = command). On an IRQ, if `ENABLE_IRQ` was issued and
  a waiter is armed, the thread posts a semaphore.
- **Wait** (`gfDeviceWaitInt(timeout_ms, int *got)`): `sem_timedwait` (or `sem_wait` for
  timeout ≤ 0). Returns 0 = IRQ (`*got = 1`), 3 = timeout, 4 = cancelled, 2 = error.
  Cancel = set a flag and post the semaphore.
- During a capture the HAL keeps the SPI clock voted while the TA drives the bus and drops
  it while waiting for the finger (fpop 4).

A Rust HAL does not have to copy the netlink design: any IRQ notification from our own
kernel driver (poll on the char device, eventfd) works, because the TA never sees it.

sysfs read by the HAL: `/sys/class/fingerprint/fingerprint/{name,type_check,adm}`,
`/sys/class/power_supply/battery/temp` (logged), `/sys/class/lcd/panel/*` (optical only).

## 9. HAL-side flows and storage

Storage (`BAuthInfoStorage`, all under `/data/vendor/biometrics` unless noted):
`fp/User_<n>/<user>_<bioidx>tmpl.dat` (templates from `Enroll_Final`),
`fp/User_<n>/info/<user>/sec_auth_id.dat`, `version.dat`, `info.dat`, `meta/calib.dat`,
`meta/cbge_*.dat`, `meta/bge_00000000.dat`, `meta/dbias.dat`, `type` / `type_check.dat`
(sensor type), `/efs/biometrics/` (factory calibration copies). The user id string is the
Android user/group id formatted by the HAL; the storepath sent with op `0x30` is this
directory. Only the templates and `sec_auth_id.dat` matter for a minimal HAL.

**Session start** (`FPBAuthService::request`, `bas 0x653B4`):
`SessionOpen(6)` → register callback, `SET_LOCKSCREEN` → read all templates of the user →
op `0x30` storepath → op `0x31` key (on failure `GetK_From_KM`, §7) → op `0x25` pre-decrypt
(templates in `0x3D400` chunks) → op `0x2F` with `sec_auth_id.dat`.

**Enroll** (`FPBAuthService::enroll`, `bas 0x56788`): power/clock on, `common_prepare`
(`Prepare` ×2 + fpop handling) → reject if `hat->user_id == 0` → op `0x31` key →
`Hat_OP(hat)` → pick a free bio index 1..4 → `Enroll_Init(user, bio_idx, 7, storepath)` →
loop { `Enroll_Do` → fpop (wait IRQ / finger up / reset) → report progress } until done →
`Enroll_Final` → write the template file → op `0x2D` save secure auth id → op `0x2E` get
authenticator id → write `sec_auth_id.dat` → power down.

**Identify** (`FPBAuthService::identify`, `bas 0x59450`): `common_prepare` → op `0x31` key
→ `Identify_Init` (templates in chunks, HAT template with the challenge) → loop {
`Identify_Do` → fpop } → `Identify_Final` → on a match: HAT from `Identify_Do`, op `0x59`
late template update (the updated template is written back), op `0x2E` authenticator id.

**Remove**: read the template file, op `0x30`, `REMOVE_TEMPLATE` op (number open), delete
the file and the `BAuthUserInfo` entry. **Enumerate**: purely from the HAL's own user-info
database (bio index list parsed with `strtok`); no TA call seen. **Authenticator id**:
op `0x2E` / `sec_auth_id.dat`. **Cancel**: cmd `0x0A` + cancel the IRQ wait.

## 10. Open questions and boot-state dependencies

1. **HAT verification (blocking for enroll).** `securefp` verifies the enroll HAT with an
   HMAC key it gets from `skeymast`/`keymaster` via `authhat`. AOSP's software gatekeeper
   signs with a different key, so `Hat_OP` should fail with `IntegrityCheckAuthToken`.
   Options to test on the tablet: (a) load `skeymast` + `authhat` and run the §7 sequence,
   then check whether Samsung's gatekeeper TA (used by `gatekeeper.mdfpp.so`) is also needed
   to produce a HAT the TA accepts; (b) find a TA path that skips the check (none seen in
   the strings); (c) run Qualcomm's `keymaster64` + QC gatekeeper TA as the gatekeeper and
   KeyMint of the build, which would make the whole chain consistent. This decides whether
   step 3 is feasible as planned; plan §25 currently assumes the HMAC only matters for
   keystore keys, which is wrong for enroll.
2. **Pre-enroll challenge.** The TA checks `hat->challenge == preEnrollChallenge` from
   `tl_generate_challenge_stub`; the control op that generates it is not identified yet
   (look in `FPBAuthService::request(unsigned int, ...)` / `runRequestThread`, `bas 0x653B4`).
   It must be returned by `IFingerprint::generateChallenge`.
3. Opcode numbers for `REMOVE_TEMPLATE`, `GET_BIOINFO_LIST`, `GET_ENROLL_SESSION`.
4. Exact field order of the Enroll_Do progress triple and of Identify_Do's score/index.
5. Meaning of `Enroll_Do`/`Identify_Do` return values `0x30` and `0x46`.
6. Whether `securefp` needs any `/efs/biometrics` calibration (`meta/calib.dat`, `cbge`) to
   capture on a capacitive GW36 (they look optical/ultrasonic-specific, but
   `common_prepare` sends whatever metadata exists).
7. **Boot state / Knox.** The TA's strings show no boot-state, warranty-bit or root check;
   keys come from `qsee_kdf` (hardware-derived), so templates enrolled under stock will not
   decrypt elsewhere but fresh ones should work. The keymaster TAs used in §7 may behave
   differently with an unlocked (orange) bootloader: untested.
8. Whether loading `securefp` from Linux works at all with the SE7 QUP owned by TZ, and
   whether the TA expects the sensor powered before session open (`readSensorType` powers
   it first) — step 1's test.
