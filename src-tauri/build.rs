fn main() {
    // ONNX Runtime is linked statically from the `ort` prebuilt binaries, so
    // there are no engine libraries to stage or bundle. The Windows build of
    // those binaries also carries the DirectML provider, which adds an import
    // of DirectML.dll. Loquara never asks for that provider, so the import is
    // delay-loaded: the DLL is only resolved if a DirectML function is called,
    // which keeps the app starting on machines whose system copy is missing or
    // too old, without shipping 18 MB of DirectML in the installer.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc")
    {
        println!("cargo:rustc-link-arg=/DELAYLOAD:DirectML.dll");
        println!("cargo:rustc-link-arg=delayimp.lib");
    }

    tauri_build::build();
}
