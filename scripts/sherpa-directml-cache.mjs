import fs from "node:fs";
import path from "node:path";

// Windows CI compiles sherpa-onnx with DirectML inside the sherpa-rs-sys build
// script. That crate lives under src-tauri, so Swatinem/rust-cache drops its
// target directory before saving, and the next job compiles the C++ again.
// The build script skips CMake when SHERPA_LIB_PATH points at a directory
// whose lib/ folder already holds the installed DLLs and import libraries.
// This script only snapshots those installed files.

const root = process.cwd();
const cacheDir = path.resolve(root, "src-tauri", ".sherpa-directml");
const cacheLib = path.join(cacheDir, "lib");

const requiredDlls = [
  "onnxruntime.dll",
  "sherpa-onnx-c-api.dll",
  "sherpa-onnx-cxx-api.dll",
  "DirectML.dll",
];

const candidateSubdirs = [["lib"], ["bin"], ["bin", "Release"], ["bin", "Debug"]];

function exists(dir) {
  return fs.existsSync(dir);
}

function fileNames(dir) {
  if (!exists(dir)) return [];
  return fs
    .readdirSync(dir)
    .filter((name) => fs.statSync(path.join(dir, name)).isFile());
}

function isEngineFile(name) {
  const lower = name.toLowerCase();
  return lower.endsWith(".dll") || lower.endsWith(".lib");
}

function isComplete(names) {
  const have = new Set(names);
  return requiredDlls.every((name) => have.has(name)) && names.some((name) => name.toLowerCase().endsWith(".lib"));
}

function outDirs() {
  const dirs = [];
  for (const profile of ["release", "debug"]) {
    const build = path.join(root, "src-tauri", "target", profile, "build");
    if (!exists(build)) continue;
    for (const name of fs.readdirSync(build)) {
      if (!name.startsWith("sherpa-rs-sys-")) continue;
      const out = path.join(build, name, "out");
      if (exists(out)) dirs.push(out);
    }
  }
  return dirs;
}

function snapshot(outDir) {
  const libDir = path.join(outDir, "lib");
  const libFiles = fileNames(libDir);
  if (isComplete(libFiles)) {
    return libFiles.map((name) => path.join(libDir, name));
  }

  const merged = new Map();
  for (const parts of candidateSubdirs) {
    const dir = path.join(outDir, ...parts);
    for (const name of fileNames(dir)) {
      if (!isEngineFile(name)) continue;
      const full = path.join(dir, name);
      const previous = merged.get(name);
      if (previous && fs.statSync(previous).size >= fs.statSync(full).size) continue;
      merged.set(name, full);
    }
  }
  if (!isComplete([...merged.keys()])) return null;
  return [...merged.values()];
}

function bestSnapshot() {
  let best = null;
  let bestMtime = -1;
  for (const outDir of outDirs()) {
    const files = snapshot(outDir);
    if (!files) continue;
    const dll = files.find((file) => path.basename(file) === "sherpa-onnx-c-api.dll");
    const mtime = fs.statSync(dll).mtimeMs;
    if (mtime >= bestMtime) {
      best = files;
      bestMtime = mtime;
    }
  }
  return best;
}

function cachedNames() {
  return fileNames(cacheLib);
}

function cacheIsComplete() {
  return isComplete(cachedNames());
}

function logCache(prefix) {
  console.error(prefix);
  for (const name of cachedNames().sort()) {
    const size = fs.statSync(path.join(cacheLib, name)).size;
    console.error(`  ${name} (${size} bytes)`);
  }
}

function capture() {
  const files = bestSnapshot();
  if (!files) {
    console.error(
      "DirectML sherpa-onnx libraries were not found under src-tauri/target. Expected onnxruntime.dll, sherpa-onnx-c-api.dll, sherpa-onnx-cxx-api.dll, DirectML.dll and an import .lib.",
    );
    process.exit(1);
  }
  fs.rmSync(cacheLib, { recursive: true, force: true });
  fs.mkdirSync(cacheLib, { recursive: true });
  for (const file of files) {
    fs.copyFileSync(file, path.join(cacheLib, path.basename(file)));
  }
  if (!cacheIsComplete()) {
    console.error("Refusing to publish an incomplete DirectML engine cache.");
    process.exit(1);
  }
  logCache(`Cached DirectML engine libraries in ${cacheDir}`);
}

function check() {
  if (!cacheIsComplete()) process.exit(1);
}

function print() {
  if (!cacheIsComplete()) {
    console.error("DirectML engine cache is missing or incomplete.");
    process.exit(1);
  }
  logCache(`Using cached DirectML engine at ${cacheDir}`);
  console.log(cacheDir);
}

const command = process.argv[2];
if (command === "capture") capture();
else if (command === "check") check();
else if (command === "print") print();
else {
  console.error("usage: node scripts/sherpa-directml-cache.mjs capture|check|print");
  process.exit(2);
}
