fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc")
        || std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows")
        || std::env::var_os("CARGO_FEATURE_VULKAN").is_none()
    {
        return;
    }

    // transcribe-cpp links vulkan-1 by name. Its CMake SDK search path
    // does not reach the final Rust link step.
    println!("cargo:rerun-if-env-changed=VULKAN_SDK");
    if let Some(sdk) = std::env::var_os("VULKAN_SDK") {
        let sdk_lib = std::path::PathBuf::from(sdk).join("Lib");
        println!("cargo:rustc-link-search=native={}", sdk_lib.display());
    }
}
