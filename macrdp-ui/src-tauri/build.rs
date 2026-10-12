fn main() {
    println!("cargo:rerun-if-env-changed=MACRDP_BUILD_REVISION");
    tauri_build::build();
}
