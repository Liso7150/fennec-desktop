from __future__ import annotations

import argparse
import os
import plistlib
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
DIST = ROOT / "dist"
VERSION_PATTERN = re.compile(r"^[0-9]+\.[0-9]+\.[0-9]+(?:[-+][0-9A-Za-z.-]+)?$")


class PackagingError(RuntimeError):
    pass


def cargo_version() -> str:
    text = (ROOT / "Cargo.toml").read_text(encoding="utf-8")
    match = re.search(r'(?ms)^\[package\].*?^version\s*=\s*"([^"]+)"', text)
    if match is None:
        raise PackagingError("Cargo.toml has no package version")
    return validate_version(match.group(1))


def validate_version(value: str) -> str:
    if VERSION_PATTERN.fullmatch(value) is None:
        raise PackagingError(f"invalid package version: {value!r}")
    return value


def normalize_arch(value: str, platform: str) -> str:
    arch = value.lower().replace("-", "_")
    if arch in {"x64", "amd64", "x86_64"}:
        return "x86_64"
    if arch in {"arm64", "aarch64"}:
        return "aarch64"
    raise PackagingError(f"unsupported {platform} architecture: {value!r}")


def installer_name(version: str, platform: str, arch: str) -> str:
    validate_version(version)
    arch = normalize_arch(arch, platform)
    if platform == "windows":
        return f"FennecDesktop-{version}-windows-{arch}-setup.exe"
    if platform == "macos":
        return f"FennecDesktop-{version}-macos-{arch}.dmg"
    if platform == "linux":
        return f"FennecDesktop-{version}-linux-{arch}.deb"
    raise PackagingError(f"unsupported platform: {platform!r}")


def run(command: list[str], *, cwd: Path | None = None) -> None:
    print(shlex.join(str(part) for part in command), flush=True)
    subprocess.run(command, cwd=cwd, check=True)


def require_file(path: Path, label: str) -> Path:
    path = path.resolve()
    if not path.is_file():
        raise PackagingError(f"{label} is missing: {path}")
    return path


def output_path(value: str | None, version: str, platform: str, arch: str) -> Path:
    path = Path(value) if value else DIST / installer_name(version, platform, arch)
    path = path.resolve()
    path.parent.mkdir(parents=True, exist_ok=True)
    return path


def validate_artifact(path: Path) -> None:
    if not path.is_file() or path.stat().st_size < 100_000:
        raise PackagingError(f"installer was not created correctly: {path}")


def find_makensis(value: str | None) -> Path:
    candidates = []
    if value:
        candidates.append(Path(value))
    found = shutil.which("makensis")
    if found:
        candidates.append(Path(found))
    candidates.extend(
        [
            Path("C:/Program Files (x86)/NSIS/makensis.exe"),
            Path("C:/Program Files/NSIS/makensis.exe"),
        ]
    )
    for candidate in candidates:
        if candidate.is_file():
            return candidate.resolve()
    raise PackagingError("makensis was not found; pass --makensis")


def build_windows(args: argparse.Namespace) -> Path:
    version = validate_version(args.version)
    arch = normalize_arch(args.arch, "windows")
    binary = require_file(Path(args.binary), "Windows GUI binary")
    output = output_path(args.output, version, "windows", arch)
    makensis = find_makensis(args.makensis)
    match = re.match(r"^([0-9]+)\.([0-9]+)\.([0-9]+)", version)
    if match is None:
        raise PackagingError(f"cannot derive Windows version resource from {version!r}")
    numeric_version = ".".join((*match.groups(), "0"))
    source_script = ROOT / "packaging" / "windows" / "fennec-desktop.nsi"
    page_bitmaps = ROOT / "packaging" / "windows" / "pages"
    for name in ("welcome.bmp", "directory.bmp", "components.bmp", "install.bmp", "finish.bmp"):
        require_file(page_bitmaps / name, f"installer page {name}")
    script_text = source_script.read_text(encoding="utf-8")
    marker = 'OutFile "${DOUT}"'
    if marker not in script_text:
        raise PackagingError("Windows installer script has no output placeholder")
    script = output.parent / f".fennec-desktop-{os.getpid()}.nsi"
    script.write_text(script_text.replace(marker, f'OutFile "{output.name}"'), encoding="utf-8")
    command = [
        str(makensis),
        "/V3",
        f"/DVERSION={version}",
        f"/DARCH={arch}",
        f"/DVI_VERSION={numeric_version}",
        f"/DBINARY={binary}",
        f"/DICON={ROOT / 'build' / 'icon.ico'}",
        f"/DWELCOME_BITMAP={ROOT / 'packaging' / 'windows' / 'welcome.bmp'}",
        f"/DHEADER_BITMAP={ROOT / 'packaging' / 'windows' / 'header.bmp'}",
        f"/DPAGE_BITMAPS={page_bitmaps}",
        str(script),
    ]
    try:
        run(command, cwd=output.parent)
    finally:
        script.unlink(missing_ok=True)
    validate_artifact(output)
    return output


def write_macos_bundle(bundle: Path, binary: Path, version: str) -> None:
    contents = bundle / "Contents"
    macos = contents / "MacOS"
    resources = contents / "Resources"
    macos.mkdir(parents=True)
    resources.mkdir(parents=True)
    executable = macos / "fennec-desktop-gui"
    shutil.copy2(binary, executable)
    executable.chmod(0o755)
    info = {
        "CFBundleDevelopmentRegion": "en",
        "CFBundleDisplayName": "Fennec",
        "CFBundleExecutable": "fennec-desktop-gui",
        "CFBundleIconFile": "icon.icns",
        "CFBundleIdentifier": "com.fennec.desktop",
        "CFBundleInfoDictionaryVersion": "6.0",
        "CFBundleName": "Fennec",
        "CFBundlePackageType": "APPL",
        "CFBundleShortVersionString": version,
        "CFBundleVersion": version,
        "LSMinimumSystemVersion": "11.0",
        "NSHighResolutionCapable": True,
        "NSPrincipalClass": "NSApplication",
    }
    with (contents / "Info.plist").open("wb") as handle:
        plistlib.dump(info, handle, sort_keys=False)
    iconset = contents / "Resources" / "icon.iconset"
    iconset.mkdir()
    icon = ROOT / "build" / "icon.png"
    for size in (16, 32, 128, 256, 512):
        for scale in (1, 2):
            pixels = size * scale
            suffix = "@2x" if scale == 2 else ""
            name = f"icon_{size}x{size}{suffix}.png"
            run(["sips", "-z", str(pixels), str(pixels), str(icon), "--out", str(iconset / name)])
    run(["iconutil", "-c", "icns", str(iconset), "-o", str(resources / "icon.icns")])
    shutil.rmtree(iconset)


def build_macos(args: argparse.Namespace) -> Path:
    version = validate_version(args.version)
    arch = normalize_arch(args.arch, "macos")
    binary = require_file(Path(args.binary), "macOS GUI binary")
    output = output_path(args.output, version, "macos", arch)
    work = Path(tempfile.mkdtemp(prefix=".fennec-dmg-", dir=output.parent))
    try:
        app = work / "Fennec.app"
        write_macos_bundle(app, binary, version)
        pkgroot = work / "pkgroot"
        shutil.copytree(app, pkgroot / "Fennec.app")
        package = work / "Install Fennec.pkg"
        run(
            [
                "pkgbuild",
                "--root",
                str(pkgroot),
                "--install-location",
                "/Applications",
                "--identifier",
                "com.fennec.desktop",
                "--version",
                version,
                str(package),
            ]
        )
        dmgroot = work / "dmg"
        dmgroot.mkdir()
        shutil.copytree(app, dmgroot / "Fennec.app")
        shutil.copy2(package, dmgroot / package.name)
        os.symlink("/Applications", dmgroot / "Applications")
        (dmgroot / "README.txt").write_text(
            "Fennec Desktop\n\n"
            "Drag Fennec.app to Applications, or run Install Fennec.pkg for the macOS installer wizard.\n"
            "On first launch, Fennec opens its local setup wizard.\n",
            encoding="utf-8",
        )
        run(
            [
                "hdiutil",
                "create",
                "-volname",
                f"Fennec Desktop {version}",
                "-srcfolder",
                str(dmgroot),
                "-ov",
                "-format",
                "UDZO",
                str(output),
            ]
        )
        validate_artifact(output)
        return output
    finally:
        shutil.rmtree(work, ignore_errors=True)


def debian_architecture(arch: str) -> str:
    return {"x86_64": "amd64", "aarch64": "arm64"}[arch]


def build_linux(args: argparse.Namespace) -> Path:
    version = validate_version(args.version)
    arch = normalize_arch(args.arch, "linux")
    binary = require_file(Path(args.binary), "Linux GUI binary")
    output = output_path(args.output, version, "linux", arch)
    work = Path(tempfile.mkdtemp(prefix=".fennec-deb-", dir=output.parent))
    try:
        pkgroot = work / "package"
        installed = pkgroot / "usr" / "lib" / "fennec-desktop" / "fennec-desktop-gui"
        installed.parent.mkdir(parents=True)
        shutil.copy2(binary, installed)
        installed.chmod(0o755)
        bindir = pkgroot / "usr" / "bin"
        bindir.mkdir(parents=True)
        os.symlink("../lib/fennec-desktop/fennec-desktop-gui", bindir / "fennec-desktop")
        applications = pkgroot / "usr" / "share" / "applications"
        applications.mkdir(parents=True)
        (applications / "fennec-desktop.desktop").write_text(
            "[Desktop Entry]\n"
            "Name=Fennec\n"
            "Comment=Local-first coding agent\n"
            "Exec=/usr/bin/fennec-desktop\n"
            "Icon=fennec-desktop\n"
            "Terminal=false\n"
            "Type=Application\n"
            "Categories=Development;Utility;\n"
            "StartupWMClass=fennec-desktop-gui\n",
            encoding="utf-8",
        )
        icons = pkgroot / "usr" / "share" / "icons" / "hicolor" / "512x512" / "apps"
        icons.mkdir(parents=True)
        shutil.copy2(ROOT / "build" / "icon.png", icons / "fennec-desktop.png")
        control_dir = pkgroot / "DEBIAN"
        control_dir.mkdir()
        installed_size = sum(path.stat().st_size for path in pkgroot.rglob("*") if path.is_file()) // 1024
        control = control_dir / "control"
        control.write_text(
            "Package: fennec-desktop\n"
            f"Version: {version}\n"
            "Section: utils\n"
            "Priority: optional\n"
            f"Architecture: {debian_architecture(arch)}\n"
            "Maintainer: Fennec Suite <noreply@users.noreply.github.com>\n"
            "Homepage: https://github.com/Liso7150/fennec-desktop\n"
            f"Installed-Size: {installed_size}\n"
            "Description: local-first coding agent with native setup\n"
            " Fennec runs the supervised coding service and inference engine locally.\n"
            " The desktop includes prerequisite checks, model selection, and setup.\n",
            encoding="utf-8",
        )
        debian = work / "debian"
        debian.mkdir()
        (debian / "control").write_text(
            "Source: fennec-desktop\n"
            "Section: utils\n"
            "Priority: optional\n"
            "Maintainer: Fennec Suite <noreply@users.noreply.github.com>\n"
            "Standards-Version: 4.6.2\n\n"
            "Package: fennec-desktop\n"
            f"Architecture: {debian_architecture(arch)}\n"
            "Description: local-first coding agent with native setup\n",
            encoding="utf-8",
        )
        result = subprocess.run(
            ["dpkg-shlibdeps", "--ignore-missing-info", "-O", "-e", str(installed)],
            cwd=work,
            check=True,
            capture_output=True,
            text=True,
        )
        if result.stdout.strip():
            with control.open("a", encoding="utf-8") as handle:
                handle.write(result.stdout)
        run(["dpkg-deb", "--build", "--root-owner-group", str(pkgroot), str(output)])
        run(["dpkg-deb", "--info", str(output)])
        run(["dpkg-deb", "--contents", str(output)])
        validate_artifact(output)
        return output
    finally:
        shutil.rmtree(work, ignore_errors=True)


def validate_release_set(dist: Path, version: str) -> list[Path]:
    validate_version(version)
    assets = []
    for platform in ("windows", "macos", "linux"):
        matches = sorted(path for path in dist.glob(f"FennecDesktop-{version}-{platform}-*") if path.is_file())
        if platform == "windows":
            matches = [path for path in matches if path.name.endswith("-setup.exe")]
        elif platform == "macos":
            matches = [path for path in matches if path.suffix == ".dmg"]
        else:
            matches = [path for path in matches if path.suffix == ".deb"]
        if len(matches) != 1:
            raise PackagingError(f"expected one {platform} installer, found {len(matches)}")
        validate_artifact(matches[0])
        assets.append(matches[0])
    return assets


def parser() -> argparse.ArgumentParser:
    result = argparse.ArgumentParser()
    commands = result.add_subparsers(dest="command", required=True)
    for platform in ("windows", "macos", "linux"):
        command = commands.add_parser(platform)
        command.add_argument("--version", default=cargo_version())
        command.add_argument("--arch", default=normalize_arch(os.uname().machine if hasattr(os, "uname") else os.environ.get("PROCESSOR_ARCHITECTURE", ""), platform))
        command.add_argument("--binary", required=True)
        command.add_argument("--output")
        if platform == "windows":
            command.add_argument("--makensis")
    validate = commands.add_parser("validate")
    validate.add_argument("--version", default=cargo_version())
    validate.add_argument("--dist", type=Path, default=DIST)
    return result


def main() -> int:
    args = parser().parse_args()
    try:
        if args.command == "windows":
            assets = [build_windows(args)]
        elif args.command == "macos":
            assets = [build_macos(args)]
        elif args.command == "linux":
            assets = [build_linux(args)]
        else:
            assets = validate_release_set(args.dist.resolve(), args.version)
        for asset in assets:
            print(asset)
        return 0
    except (PackagingError, subprocess.CalledProcessError, OSError) as error:
        print(f"packaging failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
