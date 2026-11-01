import { execFileSync } from "node:child_process";
import { lstatSync, readdirSync } from "node:fs";
import { join } from "node:path";

// Tauri signs the app binary, but not the third-party dylibs copied into
// Contents/Frameworks. An ad-hoc signature on each regular dylib is enough
// for the bundle to be signed with the same identity.
const directory = "src-tauri/lib";
for (const name of readdirSync(directory)) {
  if (!name.endsWith(".dylib")) {
    continue;
  }
  const path = join(directory, name);
  if (lstatSync(path).isSymbolicLink()) {
    // The symlink points at a regular file that is signed under its versioned
    // name; copying it preserves those bytes.
    continue;
  }
  execFileSync("codesign", ["--force", "--sign", "-", path], { stdio: "inherit" });
}
