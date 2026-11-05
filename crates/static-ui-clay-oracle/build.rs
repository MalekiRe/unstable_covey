fn main() {
    println!("cargo::rerun-if-changed=clay/clay.h");
    println!("cargo::rerun-if-changed=clay/oracle.c");
    cc::Build::new()
        .file("clay/oracle.c")
        .include("clay")
        .opt_level(3)
        // Same arithmetic as the Rust port: no fused multiply-adds.
        .flag_if_supported("-ffp-contract=off")
        .warnings(false)
        .compile("clay_oracle");
}
