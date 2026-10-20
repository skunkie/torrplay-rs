// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! What a run records about where it ran, as the benchmark protocol lists:
//! the device, the toolchains and commits behind both implementations, and
//! the network the seeders are reached through.
//!
//! Each field is detected rather than typed in, so runs cannot disagree with
//! their conditions; a field that cannot be detected is `null`, never a
//! guess. The Rust toolchain and librqbit revision are the harness's own
//! workspace's, which the Rust prototype is built from alongside it.

use std::path::Path;
use std::process::Command;

use serde::Serialize;

/// The device, toolchains, and network of a run.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Environment {
    pub os: Option<String>,
    pub kernel: Option<String>,
    pub architecture: &'static str,
    pub cpu_model: Option<String>,
    pub cores: Option<usize>,
    pub memory_bytes: Option<u64>,
    /// The cpufreq scaling governor of the first CPU, absent where the
    /// kernel exposes no frequency scaling, as in many virtual machines.
    pub cpu_frequency_governor: Option<String>,
    /// The hypervisor `systemd-detect-virt` reports, or `none`.
    pub virtualization: Option<String>,
    pub rust_toolchain: Option<&'static str>,
    pub librqbit_revision: Option<&'static str>,
    /// The Go toolchain and commit a Go TorrPlay binary was built with.
    pub go_build: Option<GoBuild>,
    /// The seeders the tracker hands out.
    pub seeder_count: Option<usize>,
    /// Every queueing discipline in the runner's network namespace, as `tc`
    /// prints them, which shows each seeder link's shaping.
    pub qdiscs: Option<Vec<String>>,
}

/// The build information Go embeds in a binary.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct GoBuild {
    pub go_version: String,
    pub revision: Option<String>,
    pub modified: Option<bool>,
}

impl Environment {
    /// Detects the environment of a run against `go_binary`, if it is Go
    /// TorrPlay, whose seeders `seeder_count` reports.
    pub fn detect(go_binary: Option<&Path>, seeder_count: Option<usize>) -> Self {
        let read = |path| std::fs::read_to_string(path).ok();
        let cpuinfo = read("/proc/cpuinfo").unwrap_or_default();
        let meminfo = read("/proc/meminfo").unwrap_or_default();
        Self {
            os: read("/etc/os-release").and_then(|text| os_name(&text)),
            kernel: read("/proc/sys/kernel/osrelease").map(|text| text.trim().to_owned()),
            architecture: std::env::consts::ARCH,
            cpu_model: cpu_model(&cpuinfo),
            cores: std::thread::available_parallelism().ok().map(usize::from),
            memory_bytes: field(&meminfo, "MemTotal")
                .and_then(|value| value.split_whitespace().next()?.parse::<u64>().ok())
                .map(|kib| kib * 1024),
            cpu_frequency_governor: read("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor")
                .map(|text| text.trim().to_owned()),
            virtualization: output("systemd-detect-virt", &[]),
            rust_toolchain: toolchain_channel(include_str!("../../../rust-toolchain.toml")),
            librqbit_revision: locked_revision(include_str!("../../../Cargo.lock"), "librqbit"),
            go_build: go_binary
                .and_then(|binary| go_build(&output("go", &["version", "-m", binary.to_str()?])?)),
            seeder_count,
            qdiscs: output_any(&["tc", "/usr/sbin/tc", "/sbin/tc"], &["qdisc", "show"]).map(
                |text| {
                    text.lines()
                        .map(|line| line.trim_end().to_owned())
                        .collect()
                },
            ),
        }
    }
}

/// The trimmed standard output of `program`, if it ran. Exit statuses are
/// not checked, since `systemd-detect-virt` exits nonzero to say `none`.
fn output(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    let text = String::from_utf8(output.stdout).ok()?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// The output of the first of `programs` that runs, as `tc` lives in
/// `/usr/sbin`, which a normal user's path often leaves out.
fn output_any(programs: &[&str], args: &[&str]) -> Option<String> {
    programs.iter().find_map(|program| output(program, args))
}

/// The value of the `name: value` line named `name`, as `/proc` files write
/// them.
fn field<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    text.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        (key.trim() == name).then(|| value.trim())
    })
}

/// `PRETTY_NAME` from os-release.
fn os_name(os_release: &str) -> Option<String> {
    os_release.lines().find_map(|line| {
        let value = line.strip_prefix("PRETTY_NAME=")?;
        Some(value.trim_matches('"').to_owned())
    })
}

/// The CPU's model name, or on Arm, where `/proc/cpuinfo` has none, its
/// implementer and part codes.
fn cpu_model(cpuinfo: &str) -> Option<String> {
    if let Some(name) = field(cpuinfo, "model name") {
        return Some(name.to_owned());
    }
    let implementer = field(cpuinfo, "CPU implementer")?;
    let part = field(cpuinfo, "CPU part")?;
    Some(format!("implementer {implementer} part {part}"))
}

/// The `channel` of a `rust-toolchain.toml`.
fn toolchain_channel(toolchain: &'static str) -> Option<&'static str> {
    toolchain.lines().find_map(|line| {
        let value = line
            .strip_prefix("channel")?
            .trim_start()
            .strip_prefix('=')?;
        Some(value.trim().trim_matches('"'))
    })
}

/// The git revision `Cargo.lock` locks package `name` to.
fn locked_revision(lock: &'static str, name: &str) -> Option<&'static str> {
    let package = lock.split("[[package]]").find(|package| {
        package
            .lines()
            .any(|line| line == format!("name = \"{name}\""))
    })?;
    let source = package
        .lines()
        .find_map(|line| line.strip_prefix("source = \"git+"))?;
    Some(source.rsplit_once('#')?.1.trim_end_matches('"'))
}

/// Parses `go version -m` output.
fn go_build(text: &str) -> Option<GoBuild> {
    let go_version = text.lines().next()?.rsplit_once(": ")?.1.trim().to_owned();
    let setting = |name: &str| {
        text.lines().find_map(|line| {
            let mut words = line.split_whitespace();
            (words.next() == Some("build"))
                .then(|| words.next()?.strip_prefix(name)?.strip_prefix('='))
                .flatten()
                .map(str::to_owned)
        })
    };
    Some(GoBuild {
        go_version,
        revision: setting("vcs.revision"),
        modified: setting("vcs.modified").and_then(|value| value.parse().ok()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_models_fall_back_to_arm_codes() {
        assert_eq!(
            cpu_model("processor\t: 0\nmodel name\t: Example CPU 9000\n").as_deref(),
            Some("Example CPU 9000")
        );
        assert_eq!(
            cpu_model("processor\t: 0\nCPU implementer\t: 0x61\nCPU part\t: 0x000\n").as_deref(),
            Some("implementer 0x61 part 0x000")
        );
        assert_eq!(cpu_model(""), None);
    }

    #[test]
    fn os_release_gives_the_pretty_name() {
        let text = "NAME=\"Example\"\nPRETTY_NAME=\"Example Linux 13 (codename)\"\n";
        assert_eq!(
            os_name(text).as_deref(),
            Some("Example Linux 13 (codename)")
        );
    }

    #[test]
    fn the_workspace_gives_the_toolchain_and_librqbit_revision() {
        assert_eq!(
            toolchain_channel("[toolchain]\nchannel = \"1.2.3\"\n"),
            Some("1.2.3")
        );
        let lock = "[[package]]\nname = \"librqbit-core\"\nsource = \"git+https://x?rev=b#bbb\"\n\n\
                    [[package]]\nname = \"librqbit\"\nversion = \"9.0.1\"\n\
                    source = \"git+https://x?rev=a#aaa\"\n";
        assert_eq!(locked_revision(lock, "librqbit"), Some("aaa"));
        let crlf = "[[package]]\r\nname = \"librqbit\"\r\nsource = \"git+https://x?rev=a#aaa\"\r\n";
        assert_eq!(
            locked_revision(crlf, "librqbit"),
            Some("aaa"),
            "Windows checkouts may end lines with CRLF"
        );
        assert!(
            Environment::detect(None, None).librqbit_revision.is_some(),
            "the workspace locks librqbit to a fork revision"
        );
    }

    #[test]
    fn go_build_information_gives_the_commit() {
        let text = "go-torrplay: go1.26.5\n\tpath\texample/cmd/torrplay\n\
                    \tbuild\tvcs=git\n\tbuild\tvcs.revision=84412a15\n\tbuild\tvcs.modified=false\n";
        assert_eq!(
            go_build(text),
            Some(GoBuild {
                go_version: "go1.26.5".to_owned(),
                revision: Some("84412a15".to_owned()),
                modified: Some(false),
            })
        );
    }
}
