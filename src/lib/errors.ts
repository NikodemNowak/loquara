import { translate, type TranslationKey } from "./i18n/lang";

/** Backend messages (exact matches) that the UI should display localized. */
const exactMessages: Record<string, TranslationKey> = {
  "Previous dictation was interrupted before audio finalization.":
    "errors.interruptedBeforeFinalize",
  "Stop dictating before deleting a model.": "errors.stopBeforeModelDelete",
  "Select another model before deleting the active one.": "errors.selectOtherBeforeModelDelete",
  "Only local WAV files are supported": "errors.onlyWav",
  "Cannot disable the mode that is currently in use.": "errors.modeInUseDisable",
  "Cannot delete the mode that is currently in use.": "errors.modeInUseDelete",
  "Could not determine the engine directory.": "errors.engineDir",
  "Could not read the download progress.": "errors.downloadProgress",
  "Could not read the download error.": "errors.downloadError",
  "The download finished with an incomplete model.": "errors.downloadIncomplete",
  "The model revision path is not a directory.": "errors.revisionNotDir",
  "Hugging Face rejected the access token.": "errors.tokenRejected",
  "The Python engine is not set up.": "errors.engineMissing",
  "Python was not found.": "errors.pythonMissing",
  "The access token is empty.": "errors.tokenEmpty",
  "The access token contains spaces.": "errors.tokenSpaces",
};

/** Backend messages with dynamic parts, matched by pattern. */
const patternMessages: Array<[
  RegExp,
  TranslationKey,
  (match: RegExpMatchArray) => Record<string, string>,
]> = [
  [/^The selected model is not ready\. Download it before retrying: (.+)\.$/, "errors.modelNotReady", (m) => ({ model: m[1] })],
  [/^Missing or empty files: (.+)$/, "errors.missingFiles", (m) => ({ files: m[1] })],
  [/^Mode "(.+)" is disabled\.$/, "errors.modeDisabled", (m) => ({ mode: m[1] })],
  [/^Invalid (.+): (.+)$/, "errors.invalidIndex", (m) => ({ name: m[1], error: m[2] })],
  [/^Could not check model artifacts: (.+)$/, "errors.artifactsCheck", (m) => ({ error: m[1] })],
  [/^Could not inspect the local model cache: (.+)$/, "errors.cacheCheck", (m) => ({ error: m[1] })],
  [/^Could not delete the model: (.+)$/, "errors.modelDeleteFailed", (m) => ({ error: m[1] })],
  [/^Model requires accepting its licence on Hugging Face: (.+)$/, "errors.modelGated", (m) => ({ repo: m[1] })],
  [/^Hugging Face rejected the access token for: (.+)$/, "errors.modelUnauthorized", (m) => ({ repo: m[1] })],
  [/^(.+) contains an invalid shard name$/, "errors.invalidShardName", (m) => ({ name: m[1] })],
  [/^(.+) contains a disallowed shard path: (.+)$/, "errors.invalidShardPath", (m) => ({ name: m[1], shard: m[2] })],
];

export function normalizeError(error: unknown, fallback = translate("errors.unknown")): string {
  const message = extractMessage(error, fallback);
  const exact = exactMessages[message];
  if (exact) return translate(exact);
  for (const [pattern, key, toParams] of patternMessages) {
    const match = message.match(pattern);
    if (match) return translate(key, toParams(match));
  }
  return message;
}

function extractMessage(error: unknown, fallback: string): string {
  if (typeof error === "string" && error.trim()) return error;
  if (error instanceof Error && error.message.trim()) return error.message;
  if (error && typeof error === "object") {
    const direct = (error as { message?: unknown }).message;
    if (typeof direct === "string" && direct.trim()) return direct;
    const nested = (error as { error?: { message?: unknown } }).error?.message;
    if (typeof nested === "string" && nested.trim()) return nested;
    try {
      const serialized = JSON.stringify(error);
      if (serialized && serialized !== "{}") return serialized;
    } catch {
      return fallback;
    }
  }
  if (error !== null && error !== undefined) {
    const primitive = String(error);
    if (primitive && primitive !== "[object Object]") return primitive;
  }
  return fallback;
}
