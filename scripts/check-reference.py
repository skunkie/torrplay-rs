#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 TorrPlay
#
# SPDX-License-Identifier: MIT

"""Verify the rendered references and their coverage of the packaged CLI."""

import argparse
import gzip
import re
import subprocess
import sys
import tempfile
import tomllib
from dataclasses import dataclass, field
from html.parser import HTMLParser
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
GENERATOR = ROOT / "scripts/build-reference.py"
SETTING_NAME = r"TORRPLAY_[A-Z0-9_]+"
CLIENT_TITLE = "TorrPlay Client Help"
CLIENT_SECTIONS = {"client-usage", "client-options", "client-connection", "troubleshooting"}


@dataclass
class DefinitionList:
    terms: list[str] = field(default_factory=list)
    tag: str = ""
    text: list[str] | None = None


class ReferenceHTML(HTMLParser):
    def __init__(self):
        super().__init__()
        self.ids = set()
        self.links = set()
        self.toc_links = set()
        self.body_links = set()
        self.terms = []
        self.definition_lists = []
        self.title = []
        self.header = []
        self.metadata = None
        self.in_toc = False
        self.in_main = False

    def handle_starttag(self, tag, attrs):
        attrs = dict(attrs)
        assert tag not in {"script", "iframe", "link", "object", "embed", "template"}, tag
        assert not any(name in {"src", "srcset"} or name.startswith("on") for name in attrs), attrs
        if tag == "nav":
            self.in_toc = True
        if tag == "title":
            self.metadata = self.title
        if tag == "header" and not self.in_main:
            self.metadata = self.header
        if tag == "main":
            self.in_main = True
        if self.in_main:
            assert "hidden" not in attrs, f"hidden reference content: {tag}"
            if "style" in attrs:
                # Pandoc represents Markdown table alignment as inline CSS.
                style = (attrs["style"] or "").strip()
                assert tag in {"th", "td"} and re.fullmatch(
                    r"text-align:\s*(?:left|center|right)\s*;?", style
                ), f"unexpected inline style on {tag}: {style}"
            if tag == "dl":
                self.definition_lists.append(DefinitionList())
            if tag in {"dt", "dd"} and self.definition_lists:
                definition = self.definition_lists[-1]
                if tag == "dt" and definition.tag == "dd":
                    definition.terms.clear()
                definition.tag = tag
                definition.text = []
        if "id" in attrs:
            anchor = attrs["id"]
            assert anchor not in self.ids, f"duplicate id {anchor}"
            self.ids.add(anchor)
        if "href" in attrs:
            link = attrs["href"]
            assert link.startswith("#"), f"nonlocal link {link}"
            self.links.add(link[1:])
            if self.in_toc:
                self.toc_links.add(link[1:])
            if self.in_main:
                self.body_links.add(link[1:])

    def handle_data(self, data):
        if self.metadata is not None:
            self.metadata.append(data)
        for definition in self.definition_lists:
            if definition.text is not None:
                definition.text.append(data)

    def handle_endtag(self, tag):
        if tag == "nav":
            self.in_toc = False
        if tag == "title" or (tag == "header" and not self.in_main):
            self.metadata = None
        if self.definition_lists:
            definition = self.definition_lists[-1]
            if tag == definition.tag and definition.text is not None:
                text = "".join(definition.text).strip()
                if tag == "dt":
                    definition.terms.append(text)
                elif text:
                    self.terms.extend(definition.terms)
                    definition.terms.clear()
                definition.text = None
            if tag == "dl":
                self.definition_lists.pop()
        if tag == "main":
            self.in_main = False


def generate(directory, *args):
    subprocess.run([sys.executable, GENERATOR, *args, directory], check=True, stdout=subprocess.DEVNULL)


def usage_names(usage):
    options = set(re.findall(r"(?<![\w-])(?:--[a-z0-9][a-z0-9-]*|-[a-zA-Z0-9])(?![\w-])", usage))
    variables = set(re.findall(rf"\b{SETTING_NAME}\b", usage))
    commands = set(re.findall(
        r"^(?:Usage:)?[ \t]+(torrplay(?: [a-z][a-z0-9-]*)+)(?![\w-])", usage, re.MULTILINE
    ))
    assert options, "no options found in CLI help"
    return options | variables | commands


def binary_usage_names(binary):
    return usage_names(subprocess.check_output([binary, "--help"], encoding="utf-8", timeout=15))


def setting_names(template):
    return set(re.findall(rf"^#?({SETTING_NAME})=", template, re.MULTILINE))


def template_settings(platform):
    template = (ROOT / f"packaging/{platform}/torrplay.env").read_text(encoding="utf-8")
    names = setting_names(template)
    assert names, f"{platform}: no template settings found"
    return names


def missing_names(terms, names):
    text = "\n".join(" ".join(term.split()) for term in terms)
    return {name for name in names if not re.search(rf"(?<![\w-]){re.escape(name)}(?![\w-])", text)}


def check_names(terms, names, label):
    missing = missing_names(terms, names)
    assert not missing, f"{label}: missing reference entries for {', '.join(sorted(missing))}"


def man_text(roff):
    return subprocess.run(
        ["groff", "-man", "-Tutf8", "-P-c", "-P-b", "-P-u", "-rLL=120n"],
        input=roff,
        encoding="utf-8",
        stdout=subprocess.PIPE,
        check=True,
    ).stdout


def man_terms(roff):
    # In Pandoc's man output, .TP starts a definition with the term on the
    # next line. Render the blocks together and ignore terms with no text below.
    parts = re.split(r"^\.(TP|TH|SH|SS|P|PP|LP)(?: [^\n]*)?\n", roff, flags=re.MULTILINE)
    blocks = [body for macro, body in zip(parts[1::2], parts[2::2]) if macro == "TP"]
    marker = "TORRPLAY-REFERENCE-DEFINITION"
    separator = f"\n.br\n.nf\n{marker}\n"
    rendered = man_text(".nf\n" + separator.join(blocks))
    entries = re.split(rf"^[ \t]*{marker}[ \t]*$", rendered, flags=re.MULTILINE)
    lines = [[line.strip() for line in entry.splitlines() if line.strip()] for entry in entries]
    return [entry[0] for entry in lines if len(entry) > 1]


def check_definition_coverage():
    parsed = ReferenceHTML()
    parsed.feed(
        "<main><!-- <dt>--hidden</dt> -->"
        "<dl><dt>--empty</dt><dd><!-- TODO --></dd>"
        "<dt><strong>--visible</strong></dt><dd>See --mentioned.</dd></dl></main>"
    )
    parsed.close()
    missing = {"--empty", "--hidden", "--mentioned"}
    names = missing | {"--visible"}
    assert missing_names(parsed.terms, names) == missing
    roff = ('.\\" .TP \\-\\-hidden\n.TP\n\\-\\-empty\n.\\" TODO\n'
            '.TP\n\\fB\\-\\-visible\\fR\nSee \\-\\-mentioned.\n')
    assert missing_names(man_terms(roff), names) == missing


def check_table_alignment():
    parsed = ReferenceHTML()
    parsed.feed(
        '<main><table><tr><th style="text-align: left;">Setting</th>'
        '<th style="text-align: right;">Default</th></tr>'
        '<tr><td style="text-align: left;">Cache</td>'
        '<td style="text-align: right;">128</td></tr></table></main>'
    )
    parsed.close()


def check_names_with_digits():
    usage = "Usage: torrplay backup-v2 [--ipv6-only]\n  -6  Use TORRPLAY_IPV6_ONLY\n"
    assert usage_names(usage) == {"torrplay backup-v2", "--ipv6-only", "-6", "TORRPLAY_IPV6_ONLY"}
    assert setting_names("#TORRPLAY_IPV6_ONLY=1\nTORRPLAY_V2=0\n") == {
        "TORRPLAY_IPV6_ONLY", "TORRPLAY_V2",
    }


def check_man_header(roff, name, version):
    command, section = name.rsplit(".", 1)
    title = command.upper().replace("-", r"\-")
    version = version.replace("-", r"\-")
    header = f'.TH "{title}" "{section}" "" "TorrPlay {version}" "TorrPlay Manual"'
    assert header in roff.splitlines(), f"{name}: expected man header {header}"


def check_man(directory, distribution, version, cli, client):
    first, second = directory / "first", directory / "second"
    args = ("man", "--distribution", distribution)
    for output in (first, second):
        generate(output, *args)
    pages = {}
    page_names = {
        "torrplay.1": cli,
        "torrplay.env.5": template_settings("linux"),
        "torrplay-client.1": client,
    }
    for name, names in page_names.items():
        page = (first / f"{name}.gz").read_bytes()
        assert page == (second / f"{name}.gz").read_bytes(), f"{name}: nondeterministic gzip"
        roff = gzip.decompress(page).decode("utf-8")
        check_man_header(roff, name, version)
        check_names(man_terms(roff), names, f"{distribution} {name}")
        pages[name] = " ".join(man_text(roff).split())
    service = pages["torrplay.1"]
    settings = pages["torrplay.env.5"]
    if distribution == "debian":
        assert "sudo ufw allow torrplay" in service and "file when it is purged" in settings
    else:
        assert "systemctl enable --now torrplay" in service and "reinstall reads the same file." in settings
        assert "ufw" not in service and "purged" not in settings


def check_html_file(path, title, label, version, sections):
    page = path.read_text(encoding="utf-8")
    if re.search(r"@import|url\s*\(", page, re.IGNORECASE):
        raise ValueError(f"{path}: CSS resource references are not allowed in offline help")
    parsed = ReferenceHTML()
    parsed.feed(page)
    parsed.close()
    if "".join(parsed.title).strip() != title:
        raise ValueError(f"{path}: expected title {title!r}")
    header = " ".join("".join(parsed.header).split())
    expected_header = f"{title} Version {version} · {label}"
    if header != expected_header:
        raise ValueError(f"{path}: expected {expected_header!r}, found {header!r}")
    if missing_links := parsed.links - parsed.ids:
        raise ValueError(f"{path}: unresolved links {sorted(missing_links)}")
    if missing_sections := sections - parsed.toc_links:
        raise ValueError(f"{path}: missing sections {sorted(missing_sections)}")
    return page, parsed


def check_html(directory, target, title, label, sections, names, version, *, override=False):
    platform, edition, distribution = target
    args = ["html", "--platform", platform, "--edition", edition]
    if distribution:
        args += ["--distribution", distribution]
    if override:
        args += ["--version", version]
    first, second = directory / "first", directory / "second"
    for output in (first, second):
        generate(output, *args)
    generated = first / "help/index.html"
    assert generated.read_bytes() == (second / "help/index.html").read_bytes(), target
    page, parsed = check_html_file(generated, title, label, version, sections)
    if platform != "linux":
        settings_links = {"allowed-hosts", "download-directory", "listen-address", "log-format"}
        assert settings_links <= parsed.body_links, f"{target}: missing setting cross-links"
        for linux_detail in ("systemctl", "/etc/torrplay", "journalctl", "/var/lib/torrplay"):
            assert linux_detail not in page, f"{target}: unexpected {linux_detail}"
    assert str(ROOT) not in page, target
    check_names(parsed.terms, names, target)


def check_debian_client_preflight(directory, version):
    original = directory / "first/help/index.html"
    command = [sys.executable, __file__, "--version", version, "--debian-client-help"]
    subprocess.run([*command, original], check=True, stdout=subprocess.DEVNULL)
    page = original.read_text(encoding="utf-8")
    fixture = directory / "network-css.html"
    for css in ('@import "https://example.invalid/help.css";',
                'body { background-image: url("https://example.invalid/help.png"); }'):
        fixture.write_text(page.replace("</head>", f"<style>{css}</style></head>", 1), encoding="utf-8")
        result = subprocess.run([*command, fixture], capture_output=True, encoding="utf-8", check=False)
        assert result.returncode == 1 and "CSS resource references" in result.stderr, css


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--torrplay", type=Path, help="built torrplay executable")
    parser.add_argument("--client", type=Path, help="built torrplay-client executable")
    parser.add_argument("--debian-client-help", type=Path, help="check staged or installed Debian client HTML")
    parser.add_argument("--version", help="expected Debian package version, including its revision")
    args = parser.parse_args()
    if args.debian_client_help:
        if not args.version or args.torrplay or args.client:
            parser.error("use --debian-client-help with --version only")
        check_html_file(args.debian_client_help, CLIENT_TITLE, "Linux (Debian)", args.version, CLIENT_SECTIONS)
        print(f"Verified Debian client help {args.version} at {args.debian_client_help}.")
        return
    if args.version:
        parser.error("--version requires --debian-client-help")
    if bool(args.torrplay) != bool(args.client):
        parser.error("supply both executables to check CLI coverage")
    check_names_with_digits()
    check_definition_coverage()
    check_table_alignment()
    cli = binary_usage_names(args.torrplay.resolve()) if args.torrplay else set()
    client = binary_usage_names(args.client.resolve()) if args.client else set()
    if args.torrplay:
        assert len(cli) > 20 and len(client) >= 5, "too few names in the CLI usage"
    desktop = cli - {"torrplay service"}
    with (ROOT / "crates/torrplay/Cargo.toml").open("rb") as manifest:
        version = tomllib.load(manifest)["package"]["version"]
    app_sections = {"startup-settings", "options", "commands", "example"}
    with tempfile.TemporaryDirectory(prefix="torrplay-reference-") as scratch:
        directory = Path(scratch)
        for distribution in ("debian", "fedora"):
            check_man(directory / f"man-{distribution}", distribution, version, cli, client)
        msi = (("windows", "service", None), "TorrPlay Service Help", "Windows (MSI)",
               CLIENT_SECTIONS | {"service-administration", "startup-settings", "options", "commands"},
               cli | client | template_settings("windows"))
        targets = [
            (("macos", "application", None), "TorrPlay Help", "macOS",
             app_sections | {"startup-recovery"}, desktop | template_settings("macos")),
            (("windows", "application", None), "TorrPlay Help", "Windows (NSIS)",
             app_sections, desktop | template_settings("windows")),
            msi,
        ]
        for distribution in ("debian", "fedora"):
            targets.append((("linux", "client", distribution), CLIENT_TITLE,
                            f"Linux ({distribution.title()})", CLIENT_SECTIONS, client))
        for target, title, label, sections, names in targets:
            check_html(directory / "-".join(part for part in target if part),
                       target, title, label, sections, names, version)
        check_debian_client_preflight(directory / "linux-client-debian", version)
        fixture_version = "2.3.5" if version == "2.3.4" else "2.3.4"
        check_html(directory / "msi-version-override", *msi, fixture_version, override=True)
    print("Verified deterministic man pages and offline HTML for every target.")
    if args.torrplay:
        print("Verified reference coverage against both packaged executables.")
    else:
        print("CLI coverage skipped because no executables were supplied.")


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError) as error:
        sys.exit(f"check-reference: {error}")
