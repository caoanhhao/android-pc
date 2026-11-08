fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    // Applies to the kernel binary and to the `cargo test` kernel alike.
    println!("cargo:rustc-link-arg=-T{dir}/linker.ld");
    println!("cargo:rerun-if-changed={dir}/linker.ld");
}
