//! Link options of the `alt` binary on Linux.
//!
//! A command that lives for a millisecond spends a visible share of it in
//! the dynamic loader. Two things the loader does at every start are
//! avoided here: relocating the executable's own image (it is linked to a
//! fixed address instead of as a position-independent executable) and
//! binding every imported function before `main` (they are bound on first
//! call). The daemons are long-lived and keep the defaults.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let var = |name: &str| std::env::var(name).unwrap_or_default();
    if var("CARGO_CFG_TARGET_OS") == "linux" && var("CARGO_CFG_TARGET_ENV") == "gnu" {
        println!("cargo:rustc-link-arg-bin=alt=-no-pie");
        println!("cargo:rustc-link-arg-bin=alt=-Wl,-z,lazy");
    }
}
