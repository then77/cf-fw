fn main() {
    for name in ["FW_APP_VERSION", "FW_SETUP_SCRIPT_SHA"] {
        println!("cargo:rerun-if-env-changed={name}");
        if let Ok(value) = std::env::var(name) {
            println!("cargo:rustc-env={name}={value}");
        }
    }
}
