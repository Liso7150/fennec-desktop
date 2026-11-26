import argparse
import plistlib
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import build_installer


class InstallerPackagingTests(unittest.TestCase):
    def test_installer_names_are_stable(self):
        self.assertEqual(
            build_installer.installer_name("1.0.0", "windows", "AMD64"),
            "FennecDesktop-1.0.0-windows-x86_64-setup.exe",
        )
        self.assertEqual(
            build_installer.installer_name("1.0.0", "macos", "arm64"),
            "FennecDesktop-1.0.0-macos-aarch64.dmg",
        )
        self.assertEqual(
            build_installer.installer_name("1.0.0", "linux", "x86_64"),
            "FennecDesktop-1.0.0-linux-x86_64.deb",
        )

    def test_version_and_architecture_are_validated(self):
        with self.assertRaises(build_installer.PackagingError):
            build_installer.validate_version("1.0.0; calc")
        with self.assertRaises(build_installer.PackagingError):
            build_installer.normalize_arch("mips", "linux")

    def test_release_validation_requires_all_three_installers(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            names = [
                "FennecDesktop-1.0.0-windows-x86_64-setup.exe",
                "FennecDesktop-1.0.0-macos-aarch64.dmg",
                "FennecDesktop-1.0.0-linux-x86_64.deb",
            ]
            for name in names:
                (root / name).write_bytes(b"x" * 100_001)
            self.assertEqual(
                [path.name for path in build_installer.validate_release_set(root, "1.0.0")],
                names,
            )
            (root / names[-1]).unlink()
            with self.assertRaises(build_installer.PackagingError):
                build_installer.validate_release_set(root, "1.0.0")


    def test_windows_wizard_pages_are_dark_fixed_size_assets(self):
        import struct

        pages = build_installer.ROOT / "packaging" / "windows" / "pages"
        for name in ("welcome.bmp", "directory.bmp", "components.bmp", "finish.bmp"):
            data = (pages / name).read_bytes()
            self.assertEqual(data[:2], b"BM", name)
            width, height = struct.unpack_from("<ii", data, 18)
            self.assertEqual((width, height), (900, 500), name)
        script = (build_installer.ROOT / "packaging" / "windows" / "fennec-desktop.nsi").read_text()
        self.assertIn('!include "nsDialogs.nsh"', script)
        self.assertIn("Page custom WelcomePage", script)
        self.assertIn("Page custom DirectoryPage", script)
        self.assertIn("Page custom ComponentsPage", script)
        self.assertIn("Page custom FinishPage", script)
        self.assertNotIn("FindWindowEx $3", script)
        self.assertNotIn("MUI_UNLANGUAGE", script)
        self.assertNotIn("nsDialogs::CreateBitmap", script)
        self.assertNotIn("nsDialogs::CreateText", script)
        self.assertNotIn("nsDialogs::CreateButton", script)
        self.assertNotIn("nsDialogs::CreateCheckbox", script)
        self.assertEqual(script.count("nsDialogs::Show"), 4)

    def test_macos_bundle_metadata_and_executable(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary = root / "fennec-desktop-gui"
            binary.write_bytes(b"test binary")
            bundle = root / "Fennec.app"
            with (
                patch.object(build_installer, "run") as run,
                patch.object(Path, "chmod") as chmod,
            ):
                build_installer.write_macos_bundle(bundle, binary, "1.0.0")
            executable = bundle / "Contents" / "MacOS" / "fennec-desktop-gui"
            self.assertEqual(executable.read_bytes(), b"test binary")
            chmod.assert_called_once_with(0o755)
            with (bundle / "Contents" / "Info.plist").open("rb") as handle:
                info = plistlib.load(handle)
            self.assertEqual(info["CFBundleIdentifier"], "com.fennec.desktop")
            self.assertEqual(info["CFBundleShortVersionString"], "1.0.0")
            self.assertEqual(info["CFBundleIconFile"], "icon.icns")
            self.assertTrue(any(call.args[0][0] == "iconutil" for call in run.call_args_list))

    def test_debian_staging_metadata_and_launcher(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary = root / "fennec-desktop-gui"
            binary.write_bytes(b"test binary")
            output = root / "FennecDesktop-1.0.0-linux-x86_64.deb"
            captured = {}

            def fake_run(command, *, cwd=None):
                if command[0] == "dpkg-deb" and "--build" in command:
                    package = Path(command[3])
                    captured["control"] = (package / "DEBIAN" / "control").read_text()
                    captured["desktop"] = (
                        package / "usr" / "share" / "applications" / "fennec-desktop.desktop"
                    ).read_text()
                    output.write_bytes(b"x" * 100_001)

            args = argparse.Namespace(
                version="1.0.0",
                arch="x86_64",
                binary=str(binary),
                output=str(output),
            )
            completed = subprocess.CompletedProcess([], 0, stdout="shlibs:Depends=libc6\n")
            with (
                patch.object(build_installer, "run", side_effect=fake_run),
                patch.object(build_installer.subprocess, "run", return_value=completed),
                patch.object(build_installer.os, "symlink") as symlink,
            ):
                self.assertEqual(build_installer.build_linux(args), output.resolve())
            self.assertIn("Architecture: amd64", captured["control"])
            self.assertIn("shlibs:Depends=libc6", captured["control"])
            self.assertIn("Exec=/usr/bin/fennec-desktop", captured["desktop"])
            symlink.assert_called_once()


if __name__ == "__main__":
    unittest.main()
