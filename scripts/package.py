"""Package a release build with its licenses (Python 3.11+)."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import plistlib
import shutil
import subprocess
import sys
import tempfile
import tomllib
import zipfile

ROOT = Path(__file__).resolve().parents[1]
APP = "Codex Usage Widget"
TARGETS = {
    "x86_64-pc-windows-msvc": "windows-x64",
    "aarch64-apple-darwin": "macos-arm64",
    "x86_64-apple-darwin": "macos-x64",
}


def build_release(target):
    # Panic messages can embed dependency source paths even when debug symbols are stripped.
    prefixes = [
        (Path.home(), "/build-user"),
        (Path(os.environ.get("CARGO_HOME", Path.home() / ".cargo")).resolve(), "/cargo"),
        (ROOT, "/src"),
    ]
    flags = [
        f"--remap-path-prefix={prefix}={replacement}"
        for path, replacement in prefixes
        for prefix in dict.fromkeys((str(path), path.as_posix()))
    ]
    environment = dict(os.environ)
    # These variables take precedence over Cargo configuration; retain any caller's flags.
    if "CARGO_ENCODED_RUSTFLAGS" in environment or "RUSTFLAGS" in environment:
        existing = (
            environment["CARGO_ENCODED_RUSTFLAGS"].split("\x1f")
            if "CARGO_ENCODED_RUSTFLAGS" in environment
            else environment["RUSTFLAGS"].split()
        )
        environment["CARGO_ENCODED_RUSTFLAGS"] = "\x1f".join([*filter(None, existing), *flags])
    configuration = f"target.{target}.rustflags = {json.dumps(flags)}"
    subprocess.run(
        ["cargo", "--config", configuration, "build", "--locked", "--release", "--target", target],
        cwd=ROOT, env=environment, check=True,
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", required=True, choices=TARGETS)
    parser.add_argument("--build", action="store_true", help="Build with source paths normalized before packaging")
    parser.add_argument("--binary", type=Path, help="Override the target-specific release binary path")
    parser.add_argument("--installer", action="store_true", help="Also create a Windows installer using Inno Setup")
    parser.add_argument("--iscc", type=Path, help="Path to the Inno Setup compiler (ISCC.exe)")
    parser.add_argument("--sign-identity", help="macOS Developer ID Application identity")
    parser.add_argument("--notary-profile", help="Existing notarytool keychain profile")
    args = parser.parse_args()
    windows = args.target.endswith("windows-msvc")
    if not windows and sys.platform != "darwin":
        parser.error("macOS packaging runs on macOS")
    if args.notary_profile and not args.sign_identity:
        parser.error("--notary-profile requires --sign-identity")
    if args.build and args.binary:
        parser.error("--build and --binary cannot be used together")
    if args.iscc and not args.installer:
        parser.error("--iscc requires --installer")
    compiler = None
    if args.installer:
        if not windows or sys.platform != "win32":
            parser.error("--installer requires a Windows target and a Windows build machine")
        candidates = [args.iscc] if args.iscc else [
            Path(found) if (found := shutil.which("ISCC.exe")) else None,
            *(
                Path(directory) / f"Inno Setup {version}" / "ISCC.exe"
                for directory in filter(None, (os.environ.get("ProgramFiles(x86)"), os.environ.get("ProgramFiles")))
                for version in (6, 7)
            ),
        ]
        compiler = next((path.resolve() for path in candidates if path and path.is_file()), None)
        if not compiler:
            parser.error("Install Inno Setup 6.7+ or specify its compiler with --iscc")
    if args.build:
        build_release(args.target)
    version = tomllib.loads((ROOT / "Cargo.toml").read_text())["package"]["version"]
    executable = "codex-usage-widget" + (".exe" if windows else "")
    binary = (args.binary or ROOT / "target" / args.target / "release" / executable).resolve()
    if not binary.is_file():
        parser.error(f"Build the release binary first: {binary}")
    dist = (ROOT / "dist").resolve()
    dist.mkdir(exist_ok=True)
    archive = dist / f"codex-usage-widget-{version}-{TARGETS[args.target]}.zip"
    outputs = [archive]
    with tempfile.TemporaryDirectory(prefix="package-", dir=dist) as temporary:
        staging = Path(temporary).resolve()
        assert staging.is_relative_to(dist)
        if windows:
            payload = staging
            shutil.copy2(binary, payload / executable)
        else:
            bundle = staging / f"{APP}.app"
            contents = bundle / "Contents"
            payload = contents / "Resources"
            payload.mkdir(parents=True)
            (contents / "MacOS").mkdir()
            app_binary = contents / "MacOS" / executable
            shutil.copy2(binary, app_binary)
            app_binary.chmod(0o755)
            shutil.copy2(ROOT / "assets/icon.icns", payload / "icon.icns")
            with (contents / "Info.plist").open("wb") as file:
                plistlib.dump({
                    "CFBundleName": APP,
                    "CFBundleDisplayName": APP,
                    "CFBundleIdentifier": "io.github.codex-usage-widget",
                    "CFBundleExecutable": executable,
                    "CFBundleIconFile": "icon.icns",
                    "CFBundlePackageType": "APPL",
                    "CFBundleShortVersionString": version,
                    "CFBundleVersion": version,
                    "LSMinimumSystemVersion": "13.0",
                    "LSUIElement": True,
                    "NSHighResolutionCapable": True,
                }, file)
        for name in ("README.md", "LICENSE", "THIRD-PARTY-LICENSES.md"):
            shutil.copy2(ROOT / name, payload / name)
        (payload / "assets").mkdir(exist_ok=True)
        shutil.copy2(ROOT / "assets/FONT-LICENSE.txt", payload / "assets/FONT-LICENSE.txt")
        shutil.copy2(ROOT / "assets/NOTICE.md", payload / "assets/NOTICE.md")
        if windows:
            with zipfile.ZipFile(archive, "w", zipfile.ZIP_DEFLATED, compresslevel=9) as output:
                for file in sorted(staging.rglob("*")):
                    if file.is_file():
                        output.write(file, file.relative_to(staging))
            if compiler:
                subprocess.run([
                    str(compiler), "/Qp", f"/DAppVersion={version}",
                    f"/DProjectDir={ROOT}", f"/DPayloadDir={payload}", f"/DInstallerOutputDir={dist}",
                    str(ROOT / "scripts/windows-installer.iss"),
                ], check=True)
                outputs.append(dist / f"codex-usage-widget-{version}-windows-x64-setup.exe")
        else:
            signing = ["codesign", "--force", "--sign", args.sign_identity or "-"]
            if args.sign_identity:
                signing.extend(["--options", "runtime", "--timestamp"])
            subprocess.run([*signing, str(bundle)], check=True)
            zip_command = ["ditto", "-c", "-k", "--sequesterRsrc", "--keepParent", str(bundle), str(archive)]
            subprocess.run(zip_command, check=True)
            if args.notary_profile:
                subprocess.run(["xcrun", "notarytool", "submit", str(archive), "--keychain-profile", args.notary_profile, "--wait"], check=True)
                subprocess.run(["xcrun", "stapler", "staple", str(bundle)], check=True)
                subprocess.run(zip_command, check=True)
            subprocess.run(["codesign", "--verify", "--strict", str(bundle)], check=True)
    for output in outputs:
        with output.open("rb") as file:
            digest = hashlib.file_digest(file, "sha256").hexdigest()
        output.with_suffix(output.suffix + ".sha256").write_text(f"{digest}  {output.name}\n", encoding="utf-8")
        print(output)


if __name__ == "__main__":
    main()
