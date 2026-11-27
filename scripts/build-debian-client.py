#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 TorrPlay
#
# SPDX-License-Identifier: MIT

"""Stage offline help or package the prebuilt Debian client."""

import argparse
import subprocess
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MANIFEST = ROOT / "desktop/Cargo.toml"
REFERENCE = ROOT / "target/reference/debian"


def package_version():
    with MANIFEST.open("rb") as manifest:
        package = tomllib.load(manifest)["package"]
    version = package["version"]
    main, separator, prerelease = version.partition("-")
    # Match cargo-deb's conversion, which also sets the service package version.
    if separator and any(char.isdigit() for char in prerelease) and not prerelease.isdigit():
        version = f"{main}~{prerelease}"
    revision = package["metadata"]["deb"].get("revision", "1")
    if revision not in ("", "0"):
        version = f"{version}-{revision}"
    return version


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("stage-help", help="render help with pinned Pandoc")
    package = commands.add_parser("package", help="check the staged help and run cargo deb --no-build")
    package.add_argument("output", type=Path)
    args = parser.parse_args()
    version = package_version()
    if args.command == "stage-help":
        subprocess.run([
            sys.executable, ROOT / "scripts/build-reference.py", "html",
            "--platform", "linux", "--edition", "client", "--distribution", "debian",
            "--version", version, REFERENCE,
        ], check=True)
        return
    subprocess.run([
        sys.executable, ROOT / "scripts/check-reference.py",
        "--debian-client-help", REFERENCE / "help/index.html", "--version", version,
    ], check=True)
    subprocess.run([
        "cargo", "deb", "--no-build", "--locked", "--manifest-path", MANIFEST,
        "--deb-version", version, "--output", args.output.resolve(),
    ], cwd=ROOT, check=True)


if __name__ == "__main__":
    try:
        main()
    except subprocess.CalledProcessError as error:
        sys.exit(error.returncode)
