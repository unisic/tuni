//! Packs the relative relocations of the `tuni` binary.
//!
//! A position-independent executable carries one 24-byte RELA entry for every
//! pointer it has to rebase at load: about twenty-three thousand here, 574 KiB
//! that are read once at exec and then stay resident for the life of the
//! window. DT_RELR writes the same thing as
//! a bitmap, 33 KiB in all, which took about half a MiB off the PSS a running
//! window is measured at. It needs glibc 2.36 at run time, which every
//! distribution shipping libadwaita 1.6 already has.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let linux_gnu = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux")
        && std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("gnu");
    if linux_gnu {
        println!("cargo:rustc-link-arg-bins=-Wl,-z,pack-relative-relocs");
    }
}
