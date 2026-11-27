#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 TorrPlay
#
# SPDX-License-Identifier: MIT

"""Assemble and render the packaged TorrPlay references with pinned Pandoc."""

import argparse
import gzip
import io
import json
import os
import re
import subprocess
import sys
import tomllib
from html import escape
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
REFERENCE = ROOT / "docs/reference"
PANDOC_VERSION = "3.12.1"
MAN_PAGES = ("torrplay.1", "torrplay.env.5", "torrplay-client.1")
DISTRIBUTIONS = ("debian", "fedora")
HTML_TARGETS = {
    ("macos", "application", None): ("macos-application", "TorrPlay Help", "macOS"),
    ("windows", "application", None): (
        "windows-application", "TorrPlay Help", "Windows (NSIS)"
    ),
    ("windows", "service", None): (
        "windows-service", "TorrPlay Service Help", "Windows (MSI)"
    ),
    ("linux", "client", "debian"): (
        "linux-client", "TorrPlay Client Help", "Linux (Debian)"
    ),
    ("linux", "client", "fedora"): (
        "linux-client", "TorrPlay Client Help", "Linux (Fedora)"
    ),
}
INCLUDE = re.compile(r"^<!-- include: ([a-z.{}-]+) -->$", re.MULTILINE)
LICENSE_HEADER = re.compile(r"\A<!--\nSPDX-FileCopyrightText:.*?\n-->\n\n", re.DOTALL)


def assemble(source, selections, parents=()):
    if source in parents:
        raise ValueError(f"reference fragment cycle at {source.name}")
    markdown = LICENSE_HEADER.sub("", source.read_text(encoding="utf-8"), count=1)

    def include(match):
        name = match[1]
        for key, value in selections.items():
            if value is not None:
                name = name.replace("{" + key + "}", value)
        if not re.fullmatch(r"[a-z-]+\.md", name):
            raise ValueError(f"invalid reference fragment name {name!r} in {source.name}")
        return assemble(REFERENCE / "fragments" / name, selections, (*parents, source)).rstrip()

    assembled = INCLUDE.sub(include, markdown)
    if "<!-- include:" in assembled:
        raise ValueError(f"invalid reference fragment directive in {source.name}")
    return assembled


def package_version():
    with (ROOT / "crates/torrplay/Cargo.toml").open("rb") as manifest:
        return tomllib.load(manifest)["package"]["version"]


def cli():
    parser = argparse.ArgumentParser(description=__doc__)
    formats = parser.add_subparsers(dest="format", required=True)
    man = formats.add_parser("man", help="generate the three gzip-compressed man pages")
    man.add_argument("--distribution", choices=DISTRIBUTIONS, default="debian")
    html = formats.add_parser("html", help="generate an offline help/index.html")
    html.add_argument("--platform", choices=("macos", "windows", "linux"), required=True)
    html.add_argument("--edition", choices=("application", "service", "client"), required=True)
    html.add_argument("--distribution", choices=DISTRIBUTIONS)
    html.add_argument("--client-mode", choices=("local",), default="local",
                      help="connection mode supported by the packaged client")
    for command in (man, html):
        command.add_argument("out_dir", type=Path)
        command.add_argument("--version", help="override the version from Cargo.toml")
        command.add_argument("--markdown", action="store_true",
                             help="write the assembled Markdown for review or checks")
    args = parser.parse_args()
    if args.format == "html":
        target = (args.platform, args.edition, args.distribution)
        if target not in HTML_TARGETS:
            parser.error("unsupported platform, edition, and distribution combination")
    version = args.version if args.version is not None else package_version()
    pattern = r"[0-9]+\.[0-9]+\.[0-9]+(?:[-+][0-9A-Za-z.+-]+)?"
    if args.format == "html" and target == ("linux", "client", "debian"):
        pattern = r"[0-9]+\.[0-9]+\.[0-9]+(?:[~+-][0-9A-Za-z.+~-]+)?"
    if not re.fullmatch(pattern, version):
        parser.error("version must be a package version such as 0.1.0")
    return args, version


def render(pandoc, markdown, *options):
    return subprocess.run(
        [pandoc, "--from=markdown", "--standalone", "--eol=lf", *options],
        input=markdown.encode("utf-8"),
        stdout=subprocess.PIPE,
        check=True,
    ).stdout


def main():
    args, version = cli()
    pandoc = os.getenv("PANDOC", "pandoc")
    if not args.markdown:
        banner = subprocess.check_output([pandoc, "--version"], encoding="utf-8")
        if banner.splitlines()[:1] != [f"pandoc {PANDOC_VERSION}"]:
            raise ValueError(f"expected Pandoc {PANDOC_VERSION} from {pandoc}")

    selections = {"distribution": args.distribution}
    if args.format == "man":
        for name in MAN_PAGES:
            markdown = assemble(ROOT / "docs/man" / f"{name}.md", selections)
            if args.markdown:
                content = markdown.encode("utf-8")
                path = args.out_dir / f"{name}.md"
            else:
                roff = render(pandoc, markdown, "--to=man", f"--metadata=footer:TorrPlay {version}")
                compressed = io.BytesIO()
                with gzip.GzipFile(filename="", fileobj=compressed, mode="wb", mtime=0) as stream:
                    stream.write(roff)
                content = compressed.getvalue()
                path = args.out_dir / f"{name}.gz"
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(content)
            print(path)
    else:
        source, title, platform = HTML_TARGETS[(args.platform, args.edition, args.distribution)]
        selections["client-mode"] = args.client_mode
        markdown = assemble(REFERENCE / f"{source}.md", selections)
        if args.markdown:
            content = markdown.encode("utf-8")
            path = args.out_dir / "help/index.md"
        else:
            # Embed the client's theme icon paths so help opens as a single file.
            icon_path = ROOT / "client/src/components/theme-icons.json"
            icons = json.loads(icon_path.read_text(encoding="utf-8"))
            content = render(
                pandoc, markdown, "--to=html5", "--toc",
                f"--template={REFERENCE / 'help.html'}",
                f"--variable=title={title}",
                f"--variable=platform={platform}",
                f"--variable=version={version}",
                *(f"--variable=icon-{name}={escape(icons[name])}" for name in ("system", "sun", "moon")),
            )
            path = args.out_dir / "help/index.html"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(content)
        print(path)


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        sys.exit(f"build-reference: {error}")
