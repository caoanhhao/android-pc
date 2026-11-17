//! nvtls-rewrite: the ELF edits of docs/waydroid-nvidia.md.
//!
//! ```text
//! nvtls-rewrite tls OUT_DIR LIB...        copy the libraries to OUT_DIR, NVIDIA's initial-exec
//!                                         TLS rewritten for nvglibc (others unchanged)
//! nvtls-rewrite add-needed IN OUT NAME    copy IN to OUT with NAME as a DT_NEEDED; prints the
//!                                         library NAME must itself need
//! ```

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use nvtls_rewrite::{add_needed, rewrite_tls};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const USAGE: &str = "usage: nvtls-rewrite tls OUT_DIR LIB...\n       \
                     nvtls-rewrite add-needed IN OUT NAME";

fn context<T, E: std::fmt::Display>(r: std::result::Result<T, E>, path: &Path) -> Result<T> {
    r.map_err(|e| format!("{}: {e}", path.display()).into())
}

/// Writes `data` to `dest` with the permissions of `src`.
fn write_like(src: &Path, dest: &Path, data: &[u8]) -> Result<()> {
    context(fs::write(dest, data), dest)?;
    let perms = context(fs::metadata(src), src)?.permissions();
    context(fs::set_permissions(dest, perms), dest)
}

fn tls(out_dir: &Path, libs: &[PathBuf]) -> Result<()> {
    context(fs::create_dir_all(out_dir), out_dir)?;
    for lib in libs {
        let name = lib.file_name().ok_or_else(|| format!("{}: no file name", lib.display()))?;
        let dest = out_dir.join(name);
        let mut data = context(fs::read(lib), lib)?;
        match context(rewrite_tls(&mut data), lib)? {
            0 => {
                context(fs::copy(lib, &dest), &dest)?;
            }
            n => {
                write_like(lib, &dest, &data)?;
                println!("{}: {n} TPOFF64 relocations rewritten", name.to_string_lossy());
            }
        }
    }
    Ok(())
}

fn needed(input: &Path, output: &Path, name: &str) -> Result<()> {
    let mut data = context(fs::read(input), input)?;
    let stands_in_for = context(add_needed(&mut data, name), input)?;
    write_like(input, output, &data)?;
    println!("{stands_in_for}");
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    let result = match args.as_slice() {
        [cmd, out_dir, libs @ ..] if cmd.as_os_str() == "tls" && !libs.is_empty() => {
            tls(out_dir, libs)
        }
        [cmd, input, output, name] if cmd.as_os_str() == "add-needed" => {
            needed(input, output, &name.to_string_lossy())
        }
        _ => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("nvtls-rewrite: {e}");
            ExitCode::FAILURE
        }
    }
}
