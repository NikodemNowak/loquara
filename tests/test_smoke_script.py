import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path


class SmokeScriptContractTests(unittest.TestCase):
    def test_missing_executable_fails_before_external_checks(self):
        hosts = [
            host
            for name in ("pwsh", "powershell.exe")
            if (host := shutil.which(name))
        ]
        self.assertTrue(hosts)
        repository = Path(__file__).resolve().parent.parent
        script = repository / "tests" / "smoke.ps1"
        for host in hosts:
            with self.subTest(host=host), tempfile.TemporaryDirectory() as directory:
                missing = Path(directory) / "missing" / "loquara.exe"
                process = subprocess.run(
                    [
                        host,
                        "-NoProfile",
                        "-ExecutionPolicy",
                        "Bypass",
                        "-File",
                        str(script),
                        "-ExePath",
                        str(missing),
                    ],
                    capture_output=True,
                    cwd=repository,
                    timeout=15,
                    check=False,
                )

                output = (process.stdout + process.stderr).decode(
                    "utf-8", errors="replace"
                )
                self.assertNotEqual(process.returncode, 0)
                self.assertIn("Missing Loquara executable", output)


if __name__ == "__main__":
    unittest.main()
