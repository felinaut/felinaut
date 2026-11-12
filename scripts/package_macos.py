#!/usr/bin/env python3
"""Build a native Felinaut .app and a Finder-configured, drag-to-Applications DMG.

Requires macOS, Python 3.11+, Rust, and Xcode command-line tools. No pip packages.
By default artifacts use an ad-hoc code signature. For other Macs, set
FELINAUT_SIGN_IDENTITY to a "Developer ID Application: …" identity, and
FELINAUT_NOTARY_PROFILE to an `xcrun notarytool store-credentials` profile.
Legacy FELIFOX_* variables are fallbacks when the corresponding FELINAUT_* is unset.
Passkeys in WKWebView need Apple's restricted browser entitlement: once Apple has
granted it, set FELINAUT_PROVISIONING_PROFILE to a Developer ID provisioning profile
that carries it and the app is signed with assets/Felinaut.entitlements.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import plistlib
import re
import shutil
import struct
import subprocess
import sys
import tempfile
import time
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
# Keep the original identity: WebKit website storage and Apple's restricted
# browser provisioning profiles are tied to this bundle ID, not the display name.
BUNDLE_ID = "io.github.casaroli.felifox"
MINIMUM_MACOS = "12.0"


def packaging_env(suffix: str) -> str:
    """Prefer Felinaut settings; an explicit empty value disables legacy settings."""
    return os.environ.get(f"FELINAUT_{suffix}", os.environ.get(f"FELIFOX_{suffix}", ""))


SIGN_IDENTITY = packaging_env("SIGN_IDENTITY")
NOTARY_PROFILE = packaging_env("NOTARY_PROFILE")
PROVISIONING_PROFILE = packaging_env("PROVISIONING_PROFILE")
ENTITLEMENTS = ROOT / "assets" / "Felinaut.entitlements"


def codesign_args(identity: str, entitlements: Path | None = None) -> list[str]:
    if not identity:
        return ["--force", "--sign", "-"]
    # Notarization requires the hardened runtime and a secure timestamp.
    args = ["--force", "--options", "runtime", "--timestamp", "--sign", identity]
    if entitlements:
        args += ["--entitlements", str(entitlements)]
    return args


def app_entitlements(provisioning_profile: str) -> Path | None:
    """Restricted entitlements are claimed only when a profile can vouch for them.

    Without a matching embedded profile macOS kills the app at launch, so an
    unprovisioned build keeps the plain hardened-runtime signature.
    """
    return ENTITLEMENTS if provisioning_profile else None


def profile_entitlements(provisioning_profile: Path) -> dict:
    """The entitlement dictionary inside a (CMS-signed) .provisionprofile."""
    decoded = run("security", "cms", "-D", "-i", provisioning_profile, capture=True)
    return plistlib.loads(decoded.stdout.encode()).get("Entitlements", {})


def run(*args: str | Path, capture: bool = False, timeout: int = 120, env=None):
    command = [str(arg) for arg in args]
    print("+", " ".join(command), flush=True)
    return subprocess.run(
        command,
        cwd=ROOT,
        check=True,
        text=True,
        stdout=subprocess.PIPE if capture else None,
        timeout=timeout,
        env=env,
    )


def display_version(source: str) -> str:
    match = re.search(r'pub const DISPLAY_VERSION:\s*&str\s*=\s*"([^"]+)";', source)
    if not match:
        raise ValueError("Could not find DISPLAY_VERSION in src/main.rs")
    return match.group(1)


def info_plist(version: str, display: str) -> dict:
    if not re.fullmatch(r"\d+\.\d+\.\d+", version):
        raise ValueError("Bundle version must have three numeric components")
    return {
        "CFBundleDevelopmentRegion": "en",
        "CFBundleName": "Felinaut",
        "CFBundleDisplayName": "Felinaut",
        "CFBundleExecutable": "felinaut",
        "CFBundleIdentifier": BUNDLE_ID,
        "CFBundlePackageType": "APPL",
        "CFBundleInfoDictionaryVersion": "6.0",
        "CFBundleShortVersionString": version,
        "CFBundleVersion": version,
        "CFBundleGetInfoString": f"Felinaut {display}",
        "CFBundleIconFile": "Felinaut.icns",
        "CFBundleSupportedPlatforms": ["MacOSX"],
        "LSMinimumSystemVersion": MINIMUM_MACOS,
        "LSApplicationCategoryType": "public.app-category.productivity",
        "NSPrincipalClass": "NSApplication",
        "NSHighResolutionCapable": True,
        # Lets macOS offer Felinaut as the default browser and hand it http/https links;
        # Apple's browser entitlements require these schemes to be declared.
        "CFBundleURLTypes": [
            {
                "CFBundleURLName": "Web address",
                "CFBundleURLSchemes": ["http", "https"],
                "CFBundleTypeRole": "Viewer",
            }
        ],
        "NSHumanReadableCopyright": "Copyright © 2026 Felinaut contributors. MIT license.",
        # Browser web content, including users' HTTP development sites, must work
        # when launched as a bundle as well as when run as a bare Cargo executable.
        "NSAppTransportSecurity": {
            "NSAllowsArbitraryLoadsInWebContent": True,
            "NSAllowsLocalNetworking": True,
        },
    }


def applescript_string(value: str | Path) -> str:
    return '"' + str(value).replace("\\", "\\\\").replace('"', '\\"') + '"'


def finder_script(mount: Path) -> str:
    folder = applescript_string(mount)
    background = applescript_string(mount / ".background" / "installer.png")
    return f"""tell application "Finder"
    set installerFolder to (POSIX file {folder}) as alias
    open installerFolder
    repeat with attempt from 1 to 30
        try
            set installerWindow to container window of installerFolder
            set current view of installerWindow to icon view
            set toolbar visible of installerWindow to false
            set statusbar visible of installerWindow to false
            set bounds of installerWindow to {{100, 100, 820, 608}}
            exit repeat
        on error messageText number errorNumber
            if attempt is 30 then error messageText number errorNumber
            delay 0.2
        end try
    end repeat
    set options to icon view options of installerWindow
    set arrangement of options to not arranged
    set icon size of options to 104
    set text size of options to 14
    set background picture of options to (POSIX file {background}) as alias
    set position of item "Felinaut.app" of installerWindow to {{190, 245}}
    set position of item "Applications" of installerWindow to {{530, 245}}
    update installerFolder without registering applications
    delay 2
    close installerWindow
end tell"""


def make_icon(art: Path, work: Path) -> Path:
    iconset = work / "Felinaut.iconset"
    iconset.mkdir()
    for logical in [16, 32, 128, 256, 512]:
        for scale in [1, 2]:
            suffix = "@2x" if scale == 2 else ""
            size = str(logical * scale)
            run(
                "sips",
                "-z",
                size,
                size,
                art / "app-icon.png",
                "--out",
                iconset / f"icon_{logical}x{logical}{suffix}.png",
                capture=True,
            )
    icon = work / "Felinaut.icns"
    run("iconutil", "--convert", "icns", "--output", icon, iconset)
    return icon


def make_app(binary: Path, icon: Path, destination: Path, version: str, display: str):
    contents = destination / "Contents"
    (contents / "MacOS").mkdir(parents=True)
    (contents / "Resources").mkdir()
    executable = contents / "MacOS" / "felinaut"
    shutil.copy2(binary, executable)
    executable.chmod(0o755)
    shutil.copy2(icon, contents / "Resources" / "Felinaut.icns")
    shutil.copy2(ROOT / "LICENSE", contents / "Resources" / "LICENSE")
    (contents / "Info.plist").write_bytes(plistlib.dumps(info_plist(version, display)))
    (contents / "PkgInfo").write_bytes(b"APPL????")
    if PROVISIONING_PROFILE:
        shutil.copy2(PROVISIONING_PROFILE, contents / "embedded.provisionprofile")
    run("plutil", "-lint", contents / "Info.plist")
    run("strip", "-x", executable)
    entitlements = app_entitlements(PROVISIONING_PROFILE)
    run("codesign", *codesign_args(SIGN_IDENTITY, entitlements), destination)
    run("codesign", "--verify", "--deep", "--strict", "--verbose=2", destination)


def portable_alias(alias: bytes, volume_name: str = "Felinaut") -> bytes:
    """Keep the HFS volume/file identity, not the builder's mount/backing-image path."""
    if len(alias) < 154 or struct.unpack_from(">H", alias, 6)[0] != 2:
        raise ValueError("Unsupported Finder background alias format")
    result = bytearray(alias[:150])
    # A portable fixed-disk alias, not an alias tied to the temporary backing image.
    struct.pack_into(">H", result, 44, 0)
    struct.pack_into(">I", result, 134, 0)
    offset = 150
    while offset + 4 <= len(alias):
        tag, length = struct.unpack_from(">hH", alias, offset)
        end = offset + 4 + length + length % 2
        if end > len(alias):
            raise ValueError("Truncated Finder alias record")
        # Replace the build mountpoint and discard backing-image / builder-home hints.
        # Keep a mountpoint record: omitting it makes Carbon drop the final path component.
        if tag == -1:
            mountpoint = f"/Volumes/{volume_name}".encode("utf-8")
            result.extend(struct.pack(">hH", 19, len(mountpoint)))
            result.extend(mountpoint + bytes(len(mountpoint) % 2))
        if tag not in (19, 20, 21):
            result.extend(alias[offset:end])
        offset = end
        if tag == -1:
            struct.pack_into(">H", result, 4, len(result))
            return bytes(result)
    raise ValueError("Finder alias is missing its end record")


def portable_finder_background(store: Path, volume_name: str = "Felinaut"):
    """Replace icvp plists in-place without changing DS_Store B-tree record sizes.

    Finder adds a backing-DMG alias that stops resolving after the temporary image
    is deleted. Retain its portable HFS identity and volume-relative image path.
    An ignored padding property keeps each binary plist exactly its original size,
    so no DS_Store allocator or B-tree offsets need to be rewritten.
    """
    data = bytearray(store.read_bytes())
    offset = 0
    updated = 0
    while (record := data.find(b"icvpblob", offset)) >= 0:
        length = struct.unpack_from(">I", data, record + 8)[0]
        start = record + 12
        end = start + length
        if end > len(data):
            raise ValueError("Invalid Finder icon-view record length")
        settings = plistlib.loads(data[start:end])
        alias = settings.get("backgroundImageAlias")
        if alias:
            settings["backgroundImageAlias"] = portable_alias(alias, volume_name)
            settings["felinautPortableLayout"] = b""
            base = len(plistlib.dumps(settings, fmt=plistlib.FMT_BINARY))
            guess = length - base
            for padding in range(max(0, guess - 32), max(0, guess) + 33):
                settings["felinautPortableLayout"] = bytes(padding)
                replacement = plistlib.dumps(settings, fmt=plistlib.FMT_BINARY)
                if len(replacement) == length:
                    data[start:end] = replacement
                    updated += 1
                    break
            else:
                raise ValueError("Could not preserve Finder view-record size")
        offset = end
    if not updated:
        raise ValueError("Finder did not save a background-image record")
    store.write_bytes(data)


def available_volume_name(volumes: Path = Path("/Volumes")) -> str:
    # Finder's AppleScript disk references are name-based. Never collide with a
    # tester's already mounted installer, and never eject someone else's volume.
    name = "Felinaut"
    suffix = 1
    while (volumes / name).exists():
        name = "Felinaut Installer" if suffix == 1 else f"Felinaut Installer {suffix}"
        suffix += 1
    return name


def make_dmg(app: Path, art: Path, icon: Path, work: Path, name: str) -> Path:
    volume_name = available_volume_name()
    payload = work / "payload"
    payload.mkdir()
    shutil.copytree(app, payload / "Felinaut.app", symlinks=True)
    (payload / "Applications").symlink_to("/Applications", target_is_directory=True)
    (payload / ".background").mkdir()
    shutil.copy2(art / "dmg-background.png", payload / ".background" / "installer.png")
    shutil.copy2(icon, payload / ".VolumeIcon.icns")
    writable = work / "layout.dmg"
    run(
        "hdiutil",
        "create",
        "-volname",
        volume_name,
        "-fs",
        "HFS+",
        "-srcfolder",
        payload,
        "-format",
        "UDRW",
        writable,
    )
    mount = work / "mount"
    mount.mkdir()
    mounted = False
    try:
        run(
            "hdiutil",
            "attach",
            "-nobrowse",
            "-noautoopen",
            "-mountpoint",
            mount,
            writable,
        )
        mounted = True
        run("SetFile", "-a", "C", mount)
        script = work / "layout.applescript"
        script.write_text(finder_script(mount))
        try:
            run("osascript", script, timeout=45)
        except (subprocess.CalledProcessError, subprocess.TimeoutExpired) as error:
            raise RuntimeError(
                "Finder layout failed. Allow your terminal/editor to control Finder in "
                "System Settings → Privacy & Security → Automation, then retry with --skip-build --overwrite. "
                "An unstyled DMG will not be published."
            ) from error
        time.sleep(2)
        if not (mount / ".DS_Store").is_file():
            raise RuntimeError(
                "Finder did not save the installer layout (.DS_Store is missing)"
            )
        portable_finder_background(mount / ".DS_Store", volume_name)
        run("sync")
    finally:
        if mounted:
            try:
                run("hdiutil", "detach", mount)
            except subprocess.CalledProcessError:
                # Only our temporary build volume is eligible for forced cleanup.
                run("hdiutil", "detach", "-force", mount)
    output = work / name
    run(
        "hdiutil",
        "convert",
        writable,
        "-format",
        "UDZO",
        "-imagekey",
        "zlib-level=9",
        "-o",
        output,
    )
    run("hdiutil", "verify", output)
    if SIGN_IDENTITY:
        run("codesign", "--force", "--timestamp", "--sign", SIGN_IDENTITY, output)
        run("codesign", "--verify", "--strict", "--verbose=2", output)
    return output


def notarize(dmg: Path, app: Path):
    # Apple scans the DMG and the app inside it; stapling lets Gatekeeper pass offline.
    run(
        "xcrun", "notarytool", "submit", dmg,
        "--keychain-profile", NOTARY_PROFILE, "--wait",
        timeout=3600,
    )
    for artifact in (dmg, app):
        run("xcrun", "stapler", "staple", artifact)
        run("xcrun", "stapler", "validate", artifact)
    run("spctl", "--assess", "--type", "execute", "--verbose=2", app)
    run("spctl", "--assess", "--type", "open", "--context", "context:primary-signature", "--verbose=2", dmg)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--skip-build",
        action="store_true",
        help="reuse the existing host-target release binary",
    )
    parser.add_argument(
        "--app-only",
        action="store_true",
        help="create only the app bundle; no Finder automation",
    )
    parser.add_argument(
        "--overwrite",
        action="store_true",
        help="replace previously generated artifacts in dist/",
    )
    args = parser.parse_args()
    if platform.system() != "Darwin":
        parser.error("macOS packaging requires macOS")
    if NOTARY_PROFILE and not SIGN_IDENTITY:
        parser.error("FELINAUT_NOTARY_PROFILE requires FELINAUT_SIGN_IDENTITY")
    if NOTARY_PROFILE and args.app_only:
        parser.error("Notarization needs the DMG; drop --app-only")
    if PROVISIONING_PROFILE:
        if not SIGN_IDENTITY:
            parser.error(
                "FELINAUT_PROVISIONING_PROFILE requires FELINAUT_SIGN_IDENTITY"
            )
        if not Path(PROVISIONING_PROFILE).is_file():
            parser.error(f"Provisioning profile not found: {PROVISIONING_PROFILE}")
        with open(ENTITLEMENTS, "rb") as file:
            claimed = plistlib.load(file)
        granted = profile_entitlements(Path(PROVISIONING_PROFILE))
        for key in claimed:
            if key not in granted:
                parser.error(
                    f"{PROVISIONING_PROFILE} does not grant {key}; macOS would refuse to launch the app"
                )
    tools = [
        "cargo",
        "rustc",
        "swift",
        "sips",
        "iconutil",
        "codesign",
        "strip",
        "plutil",
    ]
    if not args.app_only:
        tools += ["hdiutil", "osascript", "SetFile", "sync"]
    for tool in tools:
        if not shutil.which(tool):
            parser.error(f"Required tool is missing: {tool}")
    version = tomllib.loads((ROOT / "Cargo.toml").read_text())["package"]["version"]
    display = display_version((ROOT / "src" / "main.rs").read_text())
    host = next(
        line[6:]
        for line in run("rustc", "-vV", capture=True).stdout.splitlines()
        if line.startswith("host: ")
    )
    if host not in ["aarch64-apple-darwin", "x86_64-apple-darwin"]:
        parser.error(f"Unsupported host target: {host}")
    arch = "arm64" if host.startswith("aarch64") else "x86_64"
    slug = re.sub(r"[^A-Za-z0-9._-]", "-", display)
    dmg_name = f"Felinaut-{slug}-macos-{arch}.dmg"
    output = ROOT / "dist"
    app_output = output / "Felinaut.app"
    dmg_output = output / dmg_name
    artifacts = [app_output] + (
        [] if args.app_only else [dmg_output, output / "SHA256SUMS"]
    )
    if not args.overwrite:
        for artifact in artifacts:
            if artifact.exists():
                parser.error(
                    f"Output already exists: {artifact}; pass --overwrite to replace it"
                )
    metadata = json.loads(
        run(
            "cargo",
            "metadata",
            "--locked",
            "--no-deps",
            "--format-version=1",
            capture=True,
        ).stdout
    )
    target = Path(metadata["target_directory"])
    target.mkdir(parents=True, exist_ok=True)
    binary = target / host / "release" / "felinaut"
    if not args.skip_build:
        env = os.environ.copy()
        env["MACOSX_DEPLOYMENT_TARGET"] = MINIMUM_MACOS
        # Preserve build-time macro metadata on this macOS toolchain. Strip only
        # the copied final executable, never Cargo's procedural-macro libraries.
        env["CARGO_PROFILE_RELEASE_STRIP"] = "none"
        run(
            "cargo",
            "build",
            "--release",
            "--locked",
            "--bin",
            "felinaut",
            "--target",
            host,
            env=env,
            timeout=900,
        )
    if not binary.is_file():
        parser.error(f"Release executable not found: {binary}")
    output.mkdir(exist_ok=True)
    with tempfile.TemporaryDirectory(
        prefix="felinaut-package-", dir=target
    ) as temporary:
        work = Path(temporary)
        art = work / "art"
        art.mkdir()
        run(
            "swift",
            ROOT / "scripts" / "render_macos_art.swift",
            ROOT / "assets" / "felinaut-mascot.svg",
            ROOT / "assets" / "felinaut-symbol.svg",
            art,
            display,
        )
        icon = make_icon(art, work)
        app = work / "Felinaut.app"
        make_app(binary, icon, app, version, display)
        # Keep the valid app even if Finder automation is unavailable for DMG styling.
        if app_output.exists():
            shutil.rmtree(app_output)
        shutil.copytree(app, app_output, symlinks=True)
        print(f"App bundle: {app_output}", flush=True)
        if not args.app_only:
            dmg = make_dmg(app, art, icon, work, dmg_name)
            os.replace(dmg, dmg_output)
            if NOTARY_PROFILE:
                notarize(dmg_output, app_output)
            # Checksum last: stapling modifies the DMG.
            with dmg_output.open("rb") as disk_image:
                checksum = hashlib.file_digest(disk_image, "sha256").hexdigest()
            (output / "SHA256SUMS").write_text(f"{checksum}  {dmg_name}\n")
            print(f"Installer: {dmg_output}", flush=True)
    if NOTARY_PROFILE:
        print(f"Signed with {SIGN_IDENTITY}, notarized and stapled.")
    elif SIGN_IDENTITY:
        print(f"Signed with {SIGN_IDENTITY}. Not notarized: other Macs will still block it.")
    else:
        print("Local ad-hoc signature only. Not Developer ID signed or Apple-notarized.")


if __name__ == "__main__":
    try:
        main()
    except (
        subprocess.CalledProcessError,
        subprocess.TimeoutExpired,
        RuntimeError,
        ValueError,
        OSError,
    ) as error:
        print(f"Packaging failed: {error}", file=sys.stderr)
        sys.exit(1)
