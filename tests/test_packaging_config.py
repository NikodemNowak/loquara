import json
import unittest
from pathlib import Path

# Authoritative keys accepted under `bundle > windows > nsis` by the pinned
# `@tauri-apps/cli` 2.11.4 (node_modules/@tauri-apps/cli/config.schema.json,
# `NsisConfig`, `additionalProperties: false`).
#
# Note: `createDesktopShortcut` and `createStartMenuShortcut` are NOT valid keys
# in Tauri 2.11.4. The CLI validates tauri.conf.json against the bundled schema
# and exits with an error on unknown fields, so adding them breaks `tauri build`.
# The desktop-shortcut checkbox (checked by default) already ships with the NSIS
# installer via `MUI_FINISHPAGE_SHOWREADME` in the default installer.nsi template.
ALLOWED_NSIS_KEYS = frozenset(
    {
        "template",
        "headerImage",
        "sidebarImage",
        "installerIcon",
        "uninstallerIcon",
        "uninstallerHeaderImage",
        "installMode",
        "languages",
        "customLanguageFiles",
        "displayLanguageSelector",
        "compression",
        "startMenuFolder",
        "installerHooks",
        "minimumWebview2Version",
    }
)


class PackagingConfigTests(unittest.TestCase):
    def test_main_window_is_visible_on_launch_and_installs_for_current_user(self):
        repository = Path(__file__).resolve().parent.parent
        config = json.loads(
            (repository / "src-tauri" / "tauri.conf.json").read_text(
                encoding="utf-8"
            )
        )
        main = next(
            window
            for window in config["app"]["windows"]
            if window["label"] == "main"
        )

        self.assertNotIn("visible", main)
        self.assertEqual(
            config["bundle"]["windows"]["nsis"]["installMode"],
            "currentUser",
        )

    def test_nsis_config_only_uses_keys_known_to_the_pinned_cli(self):
        repository = Path(__file__).resolve().parent.parent
        config = json.loads(
            (repository / "src-tauri" / "tauri.conf.json").read_text(
                encoding="utf-8"
            )
        )
        nsis = config["bundle"]["windows"]["nsis"]

        self.assertLessEqual(set(nsis), ALLOWED_NSIS_KEYS)
        self.assertNotIn("createDesktopShortcut", nsis)
        self.assertNotIn("createStartMenuShortcut", nsis)


if __name__ == "__main__":
    unittest.main()
