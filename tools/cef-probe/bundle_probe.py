#!/usr/bin/env python3
"""Assemble an unsigned developer-only CEF probe bundle, without launching it.

Validates the exact prepared SDK offline, never downloads, and never signs,
strips, changes quarantine, or alters the SDK. Existing destinations are refused.
The upstream framework's ad-hoc signature is copied unchanged; this is not a
signed/notarized distribution or a claim that native sandbox startup works.
"""

import argparse
import hashlib
import os
import plistlib
import shutil
import stat
import struct
from pathlib import Path

import prepare_runtime

PIN = prepare_runtime.Pin()
TARGET = Path(__file__).resolve().parents[2] / "target"
EXECUTABLE = "felinaut-cef-probe"
FRAMEWORK = "Chromium Embedded Framework.framework"
HELPERS = (
    ("Helper", "helper"),
    ("Helper (GPU)", "helper.gpu"),
    ("Helper (Renderer)", "helper.renderer"),
    ("Helper (Plugin)", "helper.plugin"),
    ("Helper (Alerts)", "helper.alerts"),
)
LICENSES = ("LICENSE.txt", "CREDITS.html")
MAX_BINARY_BYTES = 2 * 1024**3


class BundleError(ValueError):
    pass


def absolute_path(path):
    path = Path(path)
    if ".." in path.parts:
        raise BundleError("Parent traversal is not permitted in input/output paths")
    return Path(os.path.abspath(path))


def no_symlink_ancestors(path):
    for component in (path, *path.parents):
        if component.is_symlink():
            raise BundleError(f"Symlink input/output ancestry refused: {component}")


def validate_destination(destination):
    destination = absolute_path(destination)
    target = absolute_path(TARGET)
    no_symlink_ancestors(destination)
    if not destination.is_relative_to(target) or destination == target:
        raise BundleError(
            "Bundle destination must be beneath this repository's ignored target/"
        )
    if destination.suffix != ".app" or destination.name == ".app":
        raise BundleError("Destination must name a new .app directory")
    if destination.exists():
        raise BundleError("Destination already exists; refusing to overwrite it")
    return destination


def executable_info(path, deadline):
    path = absolute_path(path)
    no_symlink_ancestors(path)
    prepare_runtime.regular_file(path)
    info = path.stat()
    if not 32 < info.st_size <= MAX_BINARY_BYTES:
        raise BundleError("Executable is empty, truncated or exceeds the size cap")
    if not info.st_mode & stat.S_IXUSR or info.st_mode & 0o7000:
        raise BundleError(
            "Executable must have owner execute permission and no special mode bits"
        )
    with path.open("rb") as source:
        header = source.read(32)
    # Thin little-endian MH_MAGIC_64 / CPU_TYPE_ARM64 / ARM64_ALL / MH_EXECUTE.
    if struct.unpack("<IIII", header[:16]) != (0xFEEDFACF, 0x0100000C, 0, 2):
        raise BundleError(
            "Expected a thin arm64 Mach-O executable (not a dylib or fat binary)"
        )
    return path, {
        "mode": stat.S_IMODE(info.st_mode),
        "size": info.st_size,
        "sha256": prepare_runtime.hashes(path, deadline)[1],
    }


def verify_prepared_sdk(sdk, deadline):
    sdk = absolute_path(sdk)
    no_symlink_ancestors(sdk)
    if sdk.name != "macosarm64" or sdk.parent.name != PIN.version.split("+")[0]:
        raise BundleError(
            "SDK must be the exact prepared 156.0.26/macosarm64 directory"
        )
    # Do not call prepare(): verification must not create directories or acquire SDKs.
    digest = prepare_runtime.verify_archive(sdk.parent.parent / PIN.name, PIN, deadline)
    record = prepare_runtime.verify_tree(sdk, PIN, digest, deadline)
    prefix = "Release/" + FRAMEWORK
    manifest = record["manifest"]
    if manifest.get(prefix, {}).get("kind") != "dir":
        raise BundleError("Missing prepared framework directory")
    framework = sdk / prefix
    for name, entry in manifest.items():
        if name.startswith(prefix + "/") and entry["kind"] == "symlink":
            if not (sdk / name).resolve(strict=True).is_relative_to(framework):
                raise BundleError("Framework symlink escapes the framework copy")
    for name in LICENSES:
        if manifest.get(name, {}).get("kind") != "file":
            raise BundleError(f"Publisher notice missing from verified SDK: {name}")
    return sdk, manifest


def copy_verified_file(source, destination, entry, deadline, mode=None):
    prepare_runtime.regular_file(source)
    digest, count = hashlib.sha256(), 0
    with source.open("rb") as reader, destination.open("xb") as writer:
        while data := reader.read(prepare_runtime.CHUNK):
            deadline.check()
            count += len(data)
            if count > entry["size"]:
                raise BundleError("Source file grew after preflight")
            digest.update(data)
            writer.write(data)
    if count != entry["size"] or digest.hexdigest() != entry["sha256"]:
        raise BundleError("Source content changed after preflight")
    destination.chmod(entry["mode"] if mode is None else mode)


def copy_framework(sdk, destination, manifest, deadline):
    prefix = "Release/" + FRAMEWORK
    entries = {
        name[len(prefix) + 1 :]: entry
        for name, entry in manifest.items()
        if name.startswith(prefix + "/")
    }
    destination.mkdir(mode=0o700)
    for name, entry in sorted(entries.items(), key=lambda item: item[0].count("/")):
        deadline.check()
        if entry["kind"] == "dir":
            (destination / name).mkdir(mode=0o700)
        elif entry["kind"] == "file":
            copy_verified_file(sdk / prefix / name, destination / name, entry, deadline)
        elif entry["kind"] != "symlink":
            raise BundleError("Unexpected framework entry type")
    # No file is written through a symlink, even when it precedes its target in tar order.
    for name, entry in entries.items():
        if entry["kind"] == "symlink":
            (destination / name).symlink_to(entry["target"])
    for name, entry in entries.items():
        if entry["kind"] == "dir":
            (destination / name).chmod(entry["mode"])
    destination.chmod(manifest[prefix]["mode"])


def write_plist(contents, executable, identifier, helper=False):
    value = {
        "CFBundleName": executable,
        "CFBundleDisplayName": executable,
        "CFBundleExecutable": executable,
        "CFBundleIdentifier": "io.github.casaroli.felinaut.cef-probe" + identifier,
        "CFBundleInfoDictionaryVersion": "6.0",
        "CFBundlePackageType": "APPL",
        "CFBundleVersion": "1",
        "NSHighResolutionCapable": True,
    }
    if helper:
        value["LSUIElement"] = True
    with (contents / "Info.plist").open("xb") as target:
        plistlib.dump(value, target, sort_keys=True)


def create_bundle(destination, sdk, main_binary, helper_binary):
    """Return a fresh private .app; remove only our own incomplete output on error.

    No Cargo invocation, signing, DYLD configuration, network, SDK writes or launch.
    The parent directory must be private; mkdir exclusively reserves the output,
    so a concurrently appearing destination can never be overwritten.
    """
    deadline = prepare_runtime.Deadline(180)
    destination = validate_destination(destination)
    sdk, manifest = verify_prepared_sdk(sdk, deadline)
    if destination.is_relative_to(sdk) or sdk.is_relative_to(destination):
        raise BundleError(
            "Bundle destination must not modify or contain the prepared SDK"
        )
    main_binary, main_info = executable_info(main_binary, deadline)
    helper_binary, helper_info = executable_info(helper_binary, deadline)
    if main_binary == helper_binary:
        raise BundleError("Main and helper must be distinct compiled executables")
    prefix = "Release/" + FRAMEWORK + "/"
    required = (
        sum(
            entry["size"]
            for name, entry in manifest.items()
            if name.startswith(prefix) or name in LICENSES
        )
        + main_info["size"]
        + len(HELPERS) * helper_info["size"]
        + 256 * 1024**2
    )
    if shutil.disk_usage(TARGET).free < required:
        raise BundleError(f"Insufficient bundle disk budget; require {required} bytes")
    prepare_runtime.private_directory(destination.parent)
    no_symlink_ancestors(destination)
    # Exclusive mkdir, rather than rename-over-an-empty-directory, is deliberate.
    destination.mkdir(mode=0o700)
    try:
        contents = destination / "Contents"
        macos = contents / "MacOS"
        frameworks = contents / "Frameworks"
        resources = contents / "Resources/cef"
        for directory in (macos, frameworks, resources):
            directory.mkdir(parents=True, mode=0o755)
        copy_verified_file(main_binary, macos / EXECUTABLE, main_info, deadline)
        write_plist(contents, EXECUTABLE, "")
        copy_framework(sdk, frameworks / FRAMEWORK, manifest, deadline)
        for suffix, identifier in HELPERS:
            name = EXECUTABLE + " " + suffix
            helper_contents = frameworks / (name + ".app") / "Contents"
            helper_macos = helper_contents / "MacOS"
            helper_macos.mkdir(parents=True, mode=0o755)
            copy_verified_file(
                helper_binary, helper_macos / name, helper_info, deadline
            )
            write_plist(helper_contents, name, "." + identifier, helper=True)
        for name in LICENSES:
            copy_verified_file(
                sdk / name, resources / name, manifest[name], deadline, mode=0o444
            )
        return destination
    except BaseException:
        shutil.rmtree(destination)
        raise


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--destination", type=Path, required=True)
    parser.add_argument("--sdk", type=Path, required=True)
    parser.add_argument("--main-binary", type=Path, required=True)
    parser.add_argument("--helper-binary", type=Path, required=True)
    args = parser.parse_args(argv)
    try:
        destination = create_bundle(
            args.destination, args.sdk, args.main_binary, args.helper_binary
        )
    except (OSError, ValueError, KeyError, TypeError) as error:
        parser.exit(1, f"Bundle preparation refused: {error}\n")
    print(f"PROBE_APP={destination}")
    print(
        "Developer-only, no signing or native launch performed; upstream signatures unchanged."
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
