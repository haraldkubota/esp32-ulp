//! Builds the LP core firmware in `lp/` and turns it into two files in
//! `OUT_DIR` for `src/lp_core.rs` to include:
//!
//! - `lp_core.bin`: flat image of LP SRAM, starting at `LP_RAM_START`.
//! - `lp_symbols.rs`: the address of every `ULP_*` static in the firmware, so
//!   the HP core can find the variables it shares with the LP core.

use std::env;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use object::{Object, ObjectSegment, ObjectSymbol};

const LP_TARGET: &str = "riscv32imac-unknown-none-elf";
const LP_RAM_START: u64 = 0x5000_0000;
const LP_RAM_SIZE: u64 = 16 * 1024;

fn main() {
    println!("cargo:rustc-link-arg=-Tdefmt.x");
    // make sure linkall.x is the last linker script (otherwise might cause problems with flip-link)
    println!("cargo:rustc-link-arg=-Tlinkall.x");

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let lp_dir = manifest_dir.join("lp");
    for path in ["src", "Cargo.toml", "Cargo.lock", "build.rs", "lp.x"] {
        println!("cargo:rerun-if-changed={}", lp_dir.join(path).display());
    }

    let elf_path = build_lp_firmware(&lp_dir, &out_dir.join("lp-target"));
    let elf_data = fs::read(&elf_path).unwrap();
    let elf = object::File::parse(&*elf_data).expect("LP firmware is not a valid ELF file");

    fs::write(out_dir.join("lp_core.bin"), flat_image(&elf)).unwrap();

    let mut symbols = String::new();
    for symbol in elf.symbols() {
        let name = symbol.name().unwrap();
        if name.starts_with("ULP_") {
            let addr = symbol.address();
            writeln!(
                symbols,
                "pub const {name}: *mut u32 = {addr:#010x} as *mut u32;"
            )
            .unwrap();
        }
    }
    fs::write(out_dir.join("lp_symbols.rs"), symbols).unwrap();
}

/// Runs a nested `cargo build` for the LP crate and returns the ELF path.
fn build_lp_firmware(lp_dir: &Path, target_dir: &Path) -> PathBuf {
    let mut cmd = Command::new(env::var("CARGO").unwrap());
    cmd.current_dir(lp_dir)
        .args(["build", "--release", "--target", LP_TARGET])
        .arg("--target-dir")
        .arg(target_dir);
    // Cargo passes the outer build's settings (rustflags, target, ...) to build
    // scripts via CARGO_* variables, which the nested cargo would pick up.
    // Internal __CARGO_* variables must go too: rust-analyzer runs build
    // scripts with __CARGO_TEST_CHANNEL_OVERRIDE_DO_NOT_USE_THIS=nightly, which
    // would make the nested cargo apply the `[unstable]` build-std setting from
    // .cargo/config.toml and pass -Z flags to the stable rustc.
    for (key, _) in env::vars() {
        if (key.starts_with("CARGO_") && key != "CARGO_HOME")
            || key.starts_with("__CARGO")
            || key == "RUSTFLAGS"
        {
            cmd.env_remove(key);
        }
    }

    let status = cmd.status().expect("failed to run cargo for the LP firmware");
    assert!(status.success(), "building the LP firmware failed");
    target_dir.join(LP_TARGET).join("release").join("ulp-lp")
}

/// Copies all loadable segments of `elf` into an image of LP SRAM.
/// Zero-initialised data (`.bss`) is left out; the loader clears LP SRAM
/// before copying.
fn flat_image(elf: &object::File) -> Vec<u8> {
    let mut image = Vec::new();
    for segment in elf.segments() {
        let (addr, size) = (segment.address(), segment.size());
        assert!(
            addr >= LP_RAM_START && addr + size <= LP_RAM_START + LP_RAM_SIZE,
            "LP segment at {addr:#x} ({size} bytes) is outside LP SRAM"
        );

        let data = segment.data().unwrap();
        let start = (addr - LP_RAM_START) as usize;
        let end = start + data.len();
        if image.len() < end {
            image.resize(end, 0);
        }
        image[start..end].copy_from_slice(data);
    }

    // The loader copies whole words.
    image.resize(image.len().next_multiple_of(4), 0);
    image
}
