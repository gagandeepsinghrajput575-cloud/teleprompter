fn main() {
    #[cfg(target_os = "macos")]
    {
        println!("cargo:rerun-if-changed=src/apple_speech.m");
        println!("cargo:rerun-if-changed=src/apple_speech.h");
        cc::Build::new()
            .file("src/apple_speech.m")
            .flag("-fobjc-arc")
            .compile("apple_speech");

        println!("cargo:rustc-link-lib=framework=Speech");
        println!("cargo:rustc-link-lib=framework=AVFoundation");
        println!("cargo:rustc-link-lib=framework=Foundation");
    }
    tauri_build::build();
}
