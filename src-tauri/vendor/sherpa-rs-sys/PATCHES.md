# sherpa-rs-sys 0.6.8 (vendored)

This directory is the published `sherpa-rs-sys 0.6.8` crate with one change:

- `sherpa-onnx/cmake/show-info.cmake` no longer fails when `wmic` is missing.
  Windows 11 24H2 and Windows Server 2025 removed `wmic`; the script only uses
  its output to print the build OS, so an empty result must not abort CMake.

Everything else is unchanged. The crate is MIT/Apache-2.0 licensed; see the
licence files in this directory.
