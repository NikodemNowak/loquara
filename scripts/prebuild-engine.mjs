import { execFileSync } from "node:child_process";

// Build the engine before Tauri's build script runs. On Windows this compiles
// sherpa-onnx with DirectML; on macOS and Linux it downloads the publisher's
// prebuilt CPU libraries. Either way, `src-tauri/build.rs` can then stage the
// shared libraries the bundler has to ship.
execFileSync(
  "cargo",
  [
    "build",
    "--release",
    "--manifest-path",
    "src-tauri/Cargo.toml",
    "-p",
    "sherpa-rs-sys",
  ],
  { stdio: "inherit" },
);
