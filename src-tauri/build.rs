fn main() {
    #[cfg(target_os = "macos")]
    {
        println!("cargo:rerun-if-changed=src/apple_speech.m");
        println!("cargo:rerun-if-changed=src/apple_speech.h");
        println!("cargo:rerun-if-changed=src/system_audio.m");
        cc::Build::new()
            .file("src/apple_speech.m")
            .file("src/system_audio.m")
            .flag("-fobjc-arc")
            .flag("-fblocks")
            .flag("-Wno-unguarded-availability-new")
            .flag("-Wno-unguarded-availability")
            .compile("apple_speech");

        println!("cargo:rustc-link-lib=framework=Speech");
        println!("cargo:rustc-link-lib=framework=AVFoundation");
        println!("cargo:rustc-link-lib=framework=Foundation");
        // ScreenCaptureKit is how we hear the *prospect*: it taps this Mac's own audio output
        // (the dialer plays the call), without touching the microphone stream or the dialer.
        println!("cargo:rustc-link-lib=framework=ScreenCaptureKit");
        println!("cargo:rustc-link-lib=framework=CoreMedia");
        println!("cargo:rustc-link-lib=framework=CoreAudio");
        println!("cargo:rustc-link-lib=framework=CoreFoundation");
    }
    tauri_build::build();
}
