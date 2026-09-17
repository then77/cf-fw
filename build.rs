fn main() {
    for name in ["APP_VERSION", "SETUP_SCRIPT_SHA"] {
        println!("cargo:rerun-if-env-changed={name}");
        if let Ok(value) = std::env::var(name) {
            println!("cargo:rustc-env=FW_{name}={value}");
        }
    }
}
