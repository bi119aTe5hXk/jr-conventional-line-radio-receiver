fn main() {
    if std::env::var_os("CARGO_FEATURE_SOAPY").is_some() {
        println!("cargo:rerun-if-env-changed=SOAPY_SDR_ROOT");
        if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
            let root = std::path::PathBuf::from(
                std::env::var_os("SOAPY_SDR_ROOT")
                    .expect("Set SOAPY_SDR_ROOT to a SoapySDR 0.8 MSVC SDK (include/, lib/, bin/)"),
            );
            let header = root.join("include/SoapySDR/Version.h");
            let version = std::fs::read_to_string(&header).expect("Cannot read SoapySDR Version.h");
            assert!(
                version.lines().any(|line| {
                    line.split_whitespace().collect::<Vec<_>>()
                        == ["#define", "SOAPY_SDR_ABI_VERSION", "\"0.8\""]
                }),
                "SoapySDR ABI 0.8 is required"
            );
            let library = root.join("lib/SoapySDR.lib");
            assert!(
                library.is_file(),
                "Missing MSVC import library: {}",
                library.display()
            );
            println!("cargo:rerun-if-changed={}", header.display());
            println!("cargo:rerun-if-changed={}", library.display());
            println!(
                "cargo:rustc-link-search=native={}",
                root.join("lib").display()
            );
            println!("cargo:rustc-link-lib=dylib=SoapySDR");
        } else {
            pkg_config::Config::new().range_version("0.8".."0.9").probe("SoapySDR")
                .expect("SoapySDR 0.8 SDK is required; set PKG_CONFIG_PATH or build offline tools with --no-default-features");
        }
    }
}
