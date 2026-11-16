fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let _ = build_alias::value();
}
