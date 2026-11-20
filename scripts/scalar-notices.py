#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 TorrPlay
#
# SPDX-License-Identifier: MIT

"""Collect the licenses of the packages in Scalar's pinned standalone source map."""

import base64
from concurrent.futures import ThreadPoolExecutor
import hashlib
import io
import json
from pathlib import Path
import re
import subprocess
import sys
import tarfile
from urllib.error import HTTPError
from urllib.parse import quote
from urllib.request import urlopen


def download(url):
    return subprocess.check_output(
        ["curl", "-fsSL", "--retry", "3", "--retry-all-errors", "--max-time", "120", url]
    )


def metadata(name, version):
    return json.loads(download(f"https://registry.npmjs.org/{quote(name, safe='')}/{version}"))


def scalar_revision(version):
    manifest = metadata("@scalar/api-reference", version)
    if revision := manifest.get("gitHead"):
        return revision
    dist = manifest["dist"]
    records = json.loads(download(dist["attestations"]["url"]))["attestations"]
    target_digest = base64.b64decode(dist["integrity"].removeprefix("sha512-")).hex()
    for record in records:
        if record["predicateType"] != "https://slsa.dev/provenance/v1":
            continue
        statement = json.loads(base64.b64decode(record["bundle"]["dsseEnvelope"]["payload"]))
        if not any(subject["digest"].get("sha512") == target_digest for subject in statement["subject"]):
            continue
        for source in statement["predicate"]["buildDefinition"]["resolvedDependencies"]:
            if source["uri"].startswith("git+https://github.com/scalar/scalar@"):
                return source["digest"]["gitCommit"]
    raise ValueError("Scalar's package does not identify a Git commit")


def packages(source_map):
    result = set()
    for source in json.loads(source_map)["sources"]:
        match = re.search(r"/\.pnpm/([^/]+)/node_modules/((?:@[^/]+/)?[^/]+)", source)
        if match:
            locator, name = match.groups()
            version = locator.removeprefix(name.replace("/", "+") + "@").split("_", 1)[0]
            if not re.fullmatch(r"\d+\.\d+\.\d+(?:[-+][\w.-]+)?", version):
                raise ValueError(f"unknown package version in Scalar source map: {source}")
            result.add((name, version))
        elif not re.match(r"^(?:\.\./\.\./src/|\.\./\.\./\.\./[^/]+/(?:dist|src)/)", source):
            raise ValueError(f"unknown path in Scalar source map: {source}")
    return sorted(result)


def repository_license(manifest):
    repository = manifest.get("repository", {})
    if isinstance(repository, str):
        repository = {"url": repository}
    remote = repository.get("url", "").split("#", 1)[0]
    match = re.fullmatch(r"(?:git\+)?https://github\.com/([\w.-]+/[\w.-]+?)(?:\.git)?", remote)
    revision = manifest.get("gitHead", "")
    if not match or not re.fullmatch(r"[0-9a-f]{40}", revision):
        return None
    path = repository.get("directory", "").strip("/")
    directories = [path, ""] if path else [""]
    for directory in directories:
        for license_name in ("LICENSE", "license", "LICENSE.md", "LICENSE.txt", "COPYING"):
            location = quote(f"{directory}/{license_name}".lstrip("/"), safe="/")
            base = f"https://raw.githubusercontent.com/{match[1]}/{revision}"
            try:
                with urlopen(f"{base}/{location}", timeout=30) as response:
                    return response.read().decode("utf-8").strip()
            except HTTPError as error:
                if error.code != 404:
                    raise
    return None


def notice(package):
    name, version = package
    manifest = metadata(name, version)
    dist = manifest["dist"]
    archive = download(dist["tarball"])
    integrity = "sha512-" + base64.b64encode(hashlib.sha512(archive).digest()).decode("ascii")
    if integrity != dist["integrity"]:
        raise ValueError(f"npm integrity mismatch for {name}@{version}")
    with tarfile.open(fileobj=io.BytesIO(archive), mode="r:gz") as tar:
        files = [
            member for member in tar.getmembers()
            if member.isfile()
            and Path(member.name).parent.as_posix() == "package"
            and Path(member.name).name.upper().startswith(("LICENSE", "LICENCE", "COPYING", "NOTICE"))
        ]
        content = [tar.extractfile(member).read().decode("utf-8").strip() for member in sorted(files, key=lambda m: m.name)]
    if not content:
        if license_text := repository_license(manifest):
            content.append(license_text)
        elif package == ("@replit/codemirror-css-color-picker", "6.3.0") and manifest["license"] == "MIT":
            # This release declares MIT but ships no license file in either
            # its tarball or Git tree. Preserve the published author credit.
            author = manifest["author"]["name"]
            mit_terms = Path("LICENSES/MIT.txt").read_text(encoding="utf-8").split("\n\n", 2)[2].strip()
            content.append(f"Author recorded in the package metadata: {author}\n\nMIT License\n\n{mit_terms}")
        else:
            raise ValueError(f"no license text in {name}@{version}")
    return manifest["license"], "\n\n".join(content)


def main():
    version, digest, source_map, output = sys.argv[1:]
    dependencies = packages(Path(source_map).read_bytes())
    if not dependencies:
        raise ValueError("the Scalar source map names no bundled packages")
    # Scalar's monorepo packages share the root license, which npm records
    # through the bundle package's Git commit.
    revision = scalar_revision(version)
    if not re.fullmatch(r"[0-9a-f]{40}", revision):
        raise ValueError("Scalar's package does not identify a Git commit")
    scalar_license = download(f"https://raw.githubusercontent.com/scalar/scalar/{revision}/LICENSE").decode("utf-8")
    groups = {}
    with ThreadPoolExecutor(max_workers=12) as workers:
        for (name, release), (license_id, content) in zip(dependencies, workers.map(notice, dependencies), strict=True):
            groups.setdefault((license_id, content), []).append(
                f"{name} {release}, source: https://www.npmjs.com/package/{name}/v/{release}"
            )
    lines = [
        f"Scalar standalone bundle {version}, SHA-256 {digest}", "",
        "@scalar/api-reference and its bundled Scalar workspace packages, MIT",
        f"Source: https://github.com/scalar/scalar/tree/{revision}", "", scalar_license.strip(), "",
    ]
    for (license_id, content), names in sorted(groups.items(), key=lambda item: item[1]):
        lines += ["-" * 78, license_id, *names, "", content, ""]
    text = "\n".join(line.rstrip() for line in "\n".join(lines).splitlines()) + "\n"
    Path(output).write_text(text, encoding="utf-8", newline="\n")


if __name__ == "__main__":
    main()
