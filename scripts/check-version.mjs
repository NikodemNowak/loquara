import { readFileSync } from "node:fs";

const readJson = (path) => JSON.parse(readFileSync(path, "utf8"));
const packageVersion = readJson("package.json").version;
const tauriVersion = readJson("src-tauri/tauri.conf.json").version;
const cargoVersion = readFileSync("src-tauri/Cargo.toml", "utf8").match(
  /^version\s*=\s*"([^"]+)"/m,
)?.[1];

const versions = { package: packageVersion, tauri: tauriVersion, cargo: cargoVersion };
const mismatched = Object.entries(versions).filter(([, value]) => value !== packageVersion);
if (mismatched.length > 0) {
  throw new Error(
    `Version mismatch: ${JSON.stringify(versions)}. Every manifest must carry ${packageVersion}.`,
  );
}

const tag = process.env.GITHUB_REF_NAME;
if (process.env.GITHUB_REF_TYPE === "tag" && tag !== `v${packageVersion}`) {
  throw new Error(`Tag ${tag} does not match the app version v${packageVersion}.`);
}

console.log(`Loquara ${packageVersion}`);
