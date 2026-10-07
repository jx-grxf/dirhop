fn main() {
    // The updater picks the release asset that matches the target we were built for.
    let target = std::env::var("TARGET").unwrap_or_default();
    println!("cargo:rustc-env=DIRHOP_TARGET={target}");
}
