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
    def test_main_window_starts_hidden_until_ready_and_installs_for_current_user(self):
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

        # Native setup reveals the window after state and tray initialization,
        # unless startMinimized is enabled. Config must not flash it earlier.
        self.assertIs(main["visible"], False)
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

    def test_windows_resources_are_confined_to_the_windows_config(self):
        repository = Path(__file__).resolve().parent.parent
        base = json.loads(
            (repository / "src-tauri" / "tauri.conf.json").read_text(
                encoding="utf-8"
            )
        )
        windows = json.loads(
            (repository / "src-tauri" / "tauri.windows.conf.json").read_text(
                encoding="utf-8"
            )
        )

        self.assertNotIn("resources", base["bundle"])
        self.assertEqual(windows["bundle"]["resources"], {"lib/*.dll": "./"})
        self.assertEqual(set(windows["bundle"]["targets"]), {"nsis", "msi"})

    def test_linux_bundle_ships_shared_libraries_and_xdotool(self):
        repository = Path(__file__).resolve().parent.parent
        linux = json.loads(
            (repository / "src-tauri" / "tauri.linux.conf.json").read_text(
                encoding="utf-8"
            )
        )

        self.assertEqual(linux["bundle"]["resources"], {"lib/*.so*": ""})
        self.assertEqual(set(linux["bundle"]["targets"]), {"deb", "appimage"})
        self.assertIn("xdotool", linux["bundle"]["linux"]["deb"]["depends"])

    def test_macos_bundle_carries_microphone_permission_and_engine_frameworks(self):
        repository = Path(__file__).resolve().parent.parent
        macos = json.loads(
            (repository / "src-tauri" / "tauri.macos.conf.json").read_text(
                encoding="utf-8"
            )
        )
        info = (repository / "src-tauri" / "Info.plist").read_text(encoding="utf-8")
        entitlements = (repository / "src-tauri" / "entitlements.plist").read_text(
            encoding="utf-8"
        )

        files = macos["bundle"]["macOS"]["files"]
        self.assertTrue(files)
        self.assertTrue(all(target.startswith("Frameworks/") for target in files))
        self.assertIn("lib/libonnxruntime.1.17.1.dylib", files.values())
        self.assertEqual(
            macos["bundle"]["macOS"]["entitlements"], "entitlements.plist"
        )
        self.assertIn("NSMicrophoneUsageDescription", info)
        self.assertIn("com.apple.security.device.audio-input", entitlements)
        self.assertIn("com.apple.security.automation.apple-events", entitlements)
        self.assertIn(
            "com.apple.security.cs.disable-library-validation", entitlements
        )


if __name__ == "__main__":
    unittest.main()
