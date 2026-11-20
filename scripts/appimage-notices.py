#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 TorrPlay
#
# SPDX-License-Identifier: MIT

"""Record the source packages of native executables and libraries in an AppDir."""

from collections import defaultdict
from functools import cache
import hashlib
from pathlib import Path
from platform import freedesktop_os_release
import re
import shutil
import subprocess
import sys
from urllib.parse import quote


def installed_paths():
    exact = defaultdict(set)
    relocated = defaultdict(set)
    basenames = defaultdict(set)
    for listing in Path("/var/lib/dpkg/info").glob("*.list"):
        for entry in listing.read_text(encoding="utf-8").splitlines():
            if entry.startswith(("/usr/lib/", "/lib/", "/usr/libexec/", "/usr/bin/")):
                exact[entry].add(listing.stem)
                # linuxdeploy flattens the GNU multiarch directory into usr/lib.
                normal = re.sub(r"^/(?:usr/)?lib/(?:[^/]+-linux-gnu/)?", "/usr/lib/", entry)
                relocated[normal].add((entry, listing.stem))
                basenames[entry.rsplit("/", 1)[-1]].add((entry, listing.stem))
    return exact, relocated, basenames


@cache
def binary_identity(path):
    with path.open("rb") as file:
        if file.read(4) != b"\x7fELF":
            return None
    notes = subprocess.check_output(
        ["readelf", "--notes", str(path)], env={"LC_ALL": "C"}, encoding="utf-8"
    )
    if match := re.search(r"Build ID: ([0-9a-f]+)", notes):
        return "build-id", match[1]
    with path.open("rb") as file:
        return "sha256", hashlib.file_digest(file, "sha256").hexdigest()


def native_packages(appdir):
    exact, relocated, basenames = installed_paths()
    library_cache = defaultdict(set)
    for line in subprocess.check_output(["/sbin/ldconfig", "-p"], encoding="utf-8").splitlines():
        if match := re.match(r"\s+(\S+)\s+.* => (\S+)$", line):
            name, location = match.groups()
            resolved = str(Path(location).resolve())
            library_cache[name].update((resolved, owner) for owner in exact.get(resolved, ()))
    packages = defaultdict(list)
    for path in sorted(appdir.rglob("*")):
        if path.is_symlink() or not path.is_file():
            continue
        name = path.relative_to(appdir).as_posix()
        if name in {"AppRun", "AppRun.wrapped", "usr/bin/torrplay", "usr/bin/torrplay-desktop"}:
            continue
        identity = binary_identity(path)
        if identity is None:
            continue
        # The deployed name may be an alias. Use the package owning the
        # resolved file, which can differ from the package owning the alias.
        candidates = {(f"/{name}", owner) for owner in exact.get(f"/{name}", ())}
        candidates.update(relocated.get(f"/{name}", ()))
        if name == f"usr/lib/{path.name}":
            candidates.update(library_cache[path.name])
            candidates.update(basenames[path.name])
        owners = set()
        for original, owner in candidates:
            resolved = Path(original).resolve()
            if resolved.is_file() and binary_identity(resolved) == identity:
                owners.update(exact.get(str(resolved), (owner,)))
        if len(owners) != 1:
            raise ValueError(f"cannot identify the package for {name}: {sorted(owners)}")
        packages[owners.pop()].append(name)
    if not packages:
        raise ValueError("the AppDir contains no packaged native libraries")
    return packages


def main():
    appdir = Path(sys.argv[1])
    distro = freedesktop_os_release()["ID"]
    if distro not in {"debian", "ubuntu"}:
        raise ValueError(f"no source archive configured for {distro}")
    root = appdir / "usr/share/doc"
    packages = native_packages(appdir)
    fields = r"${binary:Package}\t${Version}\t${source:Package}\t${source:Version}\n"
    metadata = subprocess.check_output(
        ["dpkg-query", "-W", f"-f={fields}", *sorted(packages)], encoding="utf-8"
    )
    lines = [
        "Native components bundled in the TorrPlay AppImage",
        "",
        f"Distribution: {distro}",
        "File paths are relative to the AppImage filesystem.",
        "",
        "AppRun and AppImage runtime: usr/share/doc/torrplay/APPIMAGE-RUNTIME-NOTICES.txt",
        "",
    ]
    sources = [line.split("\t") for line in metadata.splitlines()]
    if {binary for binary, *_ in sources} != packages.keys():
        raise ValueError("dpkg returned different packages than the AppDir inventory")
    common_licenses = set()
    for binary, version, source, source_version in sorted(sources):
        name = binary.split(":", 1)[0]
        copyright_path = root / name / "copyright"
        if copyright_path.parent.is_symlink():
            copyright_path.parent.unlink()
        copyright_path.parent.mkdir(parents=True, exist_ok=True)
        # Some distro packages share a copyright file through a symlink.
        # Copy the license text itself so the AppImage is self-contained.
        if copyright_path.is_symlink():
            copyright_path.unlink()
        original = Path("/usr/share/doc") / name / "copyright"
        shutil.copyfile(original, copyright_path)
        common_licenses.update(
            license_name.rstrip(".") for license_name in re.findall(
                r"/usr/share/common-licenses/([A-Za-z0-9.+-]+)",
                original.read_text(encoding="utf-8", errors="replace"),
            )
        )
        source_version_url = quote(source_version, safe="")
        if distro == "ubuntu":
            url = f"https://launchpad.net/ubuntu/+source/{source}/{source_version_url}"
        else:
            url = f"https://snapshot.debian.org/package/{source}/{source_version_url}/"
        lines += [
            f"{binary} {version}", f"  Source: {source} {source_version}: {url}",
            f"  Notices: usr/share/doc/{name}/copyright",
            *(f"  File: {path}" for path in packages[binary]), "",
        ]
    if common_licenses:
        directory = appdir / "usr/share/common-licenses"
        directory.mkdir(parents=True, exist_ok=True)
        for name in sorted(common_licenses):
            # Some copyright files call LGPL version 2 LGPL-2.0. Preserve
            # their referenced path using the distro's canonical license text.
            source_name = "LGPL-2" if name == "LGPL-2.0" else name
            shutil.copyfile(Path("/usr/share/common-licenses") / source_name, directory / name)
    notices = root / "torrplay/NATIVE-NOTICES.txt"
    notices.parent.mkdir(parents=True, exist_ok=True)
    notices.write_text("\n".join(lines), encoding="utf-8")


if __name__ == "__main__":
    main()
