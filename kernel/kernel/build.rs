fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    // Applies to the kernel binary and to the `cargo test` kernel alike.
    println!("cargo:rustc-link-arg=-T{dir}/linker.ld");
    println!("cargo:rerun-if-changed={dir}/linker.ld");
    bluetooth_firmware();
}

/// Intel AX211 Bluetooth firmware, built into the kernel
/// (`drivers::bluetooth::intel`): decompressed from the build host's
/// linux-firmware (`$ANDROID_PC_FIRMWARE_DIR`, default /lib/firmware).
/// Empty files when it is missing; the driver then says so.
fn bluetooth_firmware() {
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let fw_dir = std::env::var("ANDROID_PC_FIRMWARE_DIR").unwrap_or_else(|_| "/lib/firmware".into());
    println!("cargo:rerun-if-env-changed=ANDROID_PC_FIRMWARE_DIR");
    for name in ["ibt-0040-0041.sfi", "ibt-0040-0041.ddc"] {
        let src = format!("{fw_dir}/intel/{name}");
        println!("cargo:rerun-if-changed={src}");
        println!("cargo:rerun-if-changed={src}.zst");
        let data = std::fs::read(&src).ok().or_else(|| {
            let o = std::process::Command::new("zstd").args(["-dcq", &format!("{src}.zst")]).output().ok()?;
            o.status.success().then_some(o.stdout)
        });
        if data.is_none() {
            println!("cargo:warning=no {src}(.zst): the kernel will lack the Intel Bluetooth firmware");
        }
        std::fs::write(out.join(name), data.unwrap_or_default()).unwrap();
    }
}
