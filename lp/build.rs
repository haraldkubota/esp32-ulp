fn main() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    println!("cargo:rustc-link-search={dir}");
    println!("cargo:rustc-link-arg=-Tlp.x");
    println!("cargo:rerun-if-changed=lp.x");
}
