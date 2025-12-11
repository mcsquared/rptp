fn main() {
    // Ensure the linker can find `memory.x` when processing `link.x` by
    // adding this crate's directory (where `memory.x` lives) to the search path.
    println!("cargo:rerun-if-changed=memory.x");
    println!(
        "cargo:rustc-link-search={}",
        std::env::var("CARGO_MANIFEST_DIR").unwrap()
    );
}
