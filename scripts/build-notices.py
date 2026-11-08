#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 TorrPlay
#
# SPDX-License-Identifier: MIT

"""Writes the third-party notices every package installs beside TorrPlay's
license: the licenses of the Rust crates built into torrplay and the desktop
application and client, and of the web client's packages, which torrplay
embeds, with FFmpeg's LGPL for the audio decoders in two of them.

It reads the crates with cargo-about 0.9.2, from about.toml, once cargo has
fetched them, and the web client's
packages from client/node_modules, as pnpm install leaves them, with
packaging/notices.toml for what it cannot find out. It fails on a license
neither accepts and on a package without a license file. The same lockfiles
give the same file. Run it from the repository root:

    python3 scripts/build-notices.py target/notices/THIRD-PARTY-NOTICES.txt
"""

import hashlib
import json
import shutil
import subprocess
import sys
import tempfile
import tomllib
import urllib.request
from pathlib import Path

# The crates whose executables the packages ship.
MANIFESTS = ["crates/torrplay/Cargo.toml", "desktop/Cargo.toml"]
LICENSE_FILES = ("LICENSE", "LICENCE", "COPYING")
RULE = "=" * 78


def fail(message):
    sys.exit(f"build-notices: {message}")


def command(name):
    """A command's path, which on Windows finds pnpm's .cmd shim, which a bare
    name would not."""
    path = shutil.which(name)
    if path is None:
        fail(f"{name} is not installed")
    return path


def crate_source(crate):
    """Where a crate's source is: crates.io for the registry's, and the
    repository for one from Git, as cargo's package ID says."""
    package_id = crate["id"]
    if package_id.startswith("git+"):
        return package_id.removeprefix("git+").split("?")[0].split("#")[0]
    return f"https://crates.io/crates/{crate['name']}/{crate['version']}"


def normalized(text):
    """A license text with its whitespace collapsed, to find copies of it."""
    return " ".join(text.split())


def crates():
    """Each license text of the crates, with the crates under it, but TorrPlay's
    own, which its license covers."""
    groups = {}
    with tempfile.TemporaryDirectory() as directory:
        for manifest in MANIFESTS:
            output = Path(directory) / "about.json"
            # cargo-about reads the crates offline, from where cargo puts them.
            subprocess.run([command("cargo"), "fetch", "--locked", "--manifest-path", manifest], check=True)
            subprocess.run(
                [command("cargo"), "about", "generate", "--format", "json", "--locked",
                 "--offline", "--fail", "-c", "about.toml", "-m", manifest,
                 "-o", str(output)],
                check=True,
            )
            for license in json.loads(output.read_text())["licenses"]:
                group = groups.setdefault(
                    normalized(license["text"]),
                    {"ids": set(), "text": license["text"].strip("\n"), "users": set()},
                )
                group["ids"].add(license["id"])
                for user in license["used_by"]:
                    crate = user["crate"]
                    if not crate["id"].startswith("path+"):
                        group["users"].add(f"{crate['name']} {crate['version']}, source: {crate_source(crate)}")
    return [group for group in groups.values() if group["users"]]


def npm_packages(config):
    """The web client's packages that its build includes, with their paths:
    the production dependencies, as pnpm lists them, and the development ones
    the build puts in it."""
    listing = json.loads(subprocess.run(
        [command("pnpm"), "--dir", "client", "licenses", "list", "--prod", "--json"],
        check=True, capture_output=True, text=True,
    ).stdout)
    paths = {}
    for packages in listing.values():
        for package in packages:
            for path in package["paths"]:
                paths[(package["name"], Path(path))] = None
    for name in config["bundled-development"]:
        path = Path("client/node_modules", name)
        if not path.is_dir():
            fail(f"{name}, which the web client's build includes, is not installed")
        paths[(name, path.resolve())] = None
    return list(paths)


def license_text(path):
    """A package's license files, each of a dual license's among them, with
    its NOTICE file after them, which the Apache license asks to keep, or
    None."""
    files = sorted(f for f in path.iterdir() if f.name.upper().startswith(LICENSE_FILES))
    if not files:
        return None
    files += sorted(f for f in path.iterdir() if f.name.upper().startswith("NOTICE"))
    return "\n\n".join(f.read_text(encoding="utf-8").strip("\n") for f in files)


def web_client(config):
    """Each license text of the web client's packages, with the packages under
    it, and the versions of the packages FFmpeg's notice names."""
    packages = npm_packages(config)
    by_name = {name: path for name, path in packages}
    groups = {}
    versions = {}
    for name, path in packages:
        manifest = json.loads((path / "package.json").read_text(encoding="utf-8"))
        version = manifest["version"]
        versions[name] = version
        declared = manifest.get("license")
        if declared not in config["accepted"]:
            fail(f"{name} {version} has the license {declared}, which packaging/notices.toml does not accept")
        text = license_text(path)
        if text is None:
            sibling = config["license-from"].get(name)
            if sibling is None or sibling not in by_name:
                fail(f"{name} {version} has no license file, and packaging/notices.toml names none for it")
            text = license_text(by_name[sibling])
        repository = manifest.get("repository")
        if isinstance(repository, dict):
            repository = repository.get("url")
        source = (repository or f"https://www.npmjs.com/package/{name}").removeprefix("git+")
        group = groups.setdefault(normalized(text), {"ids": set(), "text": text, "users": set()})
        group["ids"].add(declared)
        group["users"].add(f"{name} {version}, source: {source}")
    return list(groups.values()), versions


def lgpl(config):
    """The LGPL's text, checked against the pinned SHA-256."""
    request = urllib.request.Request(config["url"], headers={"User-Agent": "torrplay-build-notices"})
    with urllib.request.urlopen(request, timeout=60) as response:
        body = response.read()
    if hashlib.sha256(body).hexdigest() != config["sha256"]:
        fail(f"{config['url']} is not the LGPL text packaging/notices.toml pins")
    return body.decode("utf-8").strip("\n")


def section(title, groups):
    lines = [RULE, title, RULE, ""]
    for group in sorted(groups, key=lambda g: (sorted(g["ids"]), sorted(g["users"]))):
        lines.append(" OR ".join(sorted(group["ids"])) + ", for:")
        lines.extend(f"  {user}" for user in sorted(group["users"]))
        lines += ["", group["text"], "", "-" * 78, ""]
    return lines


def main():
    if len(sys.argv) != 2:
        sys.exit(f"usage: {sys.argv[0]} OUT")
    config = tomllib.loads(Path("packaging/notices.toml").read_text(encoding="utf-8"))
    web_groups, versions = web_client(config["npm"])
    ffmpeg = config["ffmpeg"]
    missing = [name for name in ffmpeg["packages"] if name not in versions]
    if missing:
        fail(f"the web client no longer includes {', '.join(missing)}; update packaging/notices.toml")
    mediabunny = sorted({versions[name] for name in ffmpeg["packages"]})
    if len(mediabunny) != 1:
        fail(f"{', '.join(ffmpeg['packages'])} have different versions: {mediabunny}")

    lines = [
        "TorrPlay's third-party notices",
        "",
        "TorrPlay is distributed under the MIT License, in its license file beside",
        "this one. It includes the software below, under the licenses that follow.",
        "",
        RULE, "FFmpeg", RULE, "",
        ffmpeg["notice"].format(version=mediabunny[0]), "",
    ]
    lines += section("The web client's packages", web_groups)
    lines += section("Rust crates", crates())
    # Some packages' license files are the license's short notice alone, as
    # Video.js's, which refers to the whole text the license asks to give.
    apache = Path("LICENSES/Apache-2.0.txt").read_text(encoding="utf-8").strip("\n")
    lines += [RULE, "Apache License, version 2.0", RULE, "", apache, ""]
    lines += [RULE, "GNU Lesser General Public License, version 2.1", RULE, "", lgpl(ffmpeg["lgpl"]), ""]

    out = Path(sys.argv[1])
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text("\n".join(lines), encoding="utf-8")
    print(out)


if __name__ == "__main__":
    main()
