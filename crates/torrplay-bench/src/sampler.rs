// SPDX-FileCopyrightText: 2026 TorrPlay
//
// SPDX-License-Identifier: MIT

//! The server process's resident set size and CPU time, read from Linux's
//! `/proc`, which is where the benchmark protocol runs.

use std::fs;

/// One reading of a process.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ProcessSample {
    /// Resident set size.
    pub rss_bytes: u64,
    /// User CPU time.
    pub user_seconds: f64,
    /// System CPU time.
    pub system_seconds: f64,
}

/// Reads a process's resident set size and CPU time, with `ticks_per_second`
/// the kernel's clock tick rate, `getconf CLK_TCK`.
pub fn read(pid: u32, ticks_per_second: f64) -> anyhow::Result<ProcessSample> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let status = fs::read_to_string(format!("/proc/{pid}/status"))?;
    parse(&stat, &status, ticks_per_second)
}

/// Parses `/proc/<pid>/stat` and `/proc/<pid>/status`.
pub fn parse(stat: &str, status: &str, ticks_per_second: f64) -> anyhow::Result<ProcessSample> {
    // The command name, in parentheses, may contain spaces; the fields
    // after it are space separated, starting with field 3.
    let after_name = stat
        .rsplit_once(')')
        .map(|(_, rest)| rest)
        .ok_or_else(|| anyhow::anyhow!("no command name in the stat line"))?;
    let fields: Vec<&str> = after_name.split_whitespace().collect();
    let ticks = |field: usize| -> anyhow::Result<f64> {
        let text = fields
            .get(field - 3)
            .ok_or_else(|| anyhow::anyhow!("the stat line has no field {field}"))?;
        Ok(text.parse::<u64>()? as f64 / ticks_per_second)
    };
    let rss_kib = status
        .lines()
        .find_map(|line| line.strip_prefix("VmRSS:"))
        .and_then(|value| value.split_whitespace().next())
        .ok_or_else(|| anyhow::anyhow!("no VmRSS in the status"))?
        .parse::<u64>()?;
    Ok(ProcessSample {
        rss_bytes: rss_kib * 1024,
        user_seconds: ticks(14)?,
        system_seconds: ticks(15)?,
    })
}

/// The kernel's clock tick rate.
pub fn ticks_per_second() -> anyhow::Result<f64> {
    let output = std::process::Command::new("getconf")
        .arg("CLK_TCK")
        .output()?;
    anyhow::ensure!(output.status.success(), "getconf CLK_TCK failed");
    Ok(String::from_utf8(output.stdout)?.trim().parse()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stat_and_status_lines_parse() {
        let stat = "4242 (tokio (x) y) S 1 4242 4242 0 -1 4194560 2741 0 0 0 250 125 0 0 20 0 9 0 \
                    123 456 789";
        let status = "Name:\ttorrplay\nVmPeak:\t  999 kB\nVmRSS:\t   20480 kB\nThreads:\t9\n";
        let sample = parse(stat, status, 100.0).unwrap();
        assert_eq!(sample.rss_bytes, 20 * 1024 * 1024);
        assert_eq!(sample.user_seconds, 2.5);
        assert_eq!(sample.system_seconds, 1.25);
        assert!(parse("garbage", status, 100.0).is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn this_process_can_be_read() {
        let sample = read(std::process::id(), ticks_per_second().unwrap()).unwrap();
        assert!(sample.rss_bytes > 0);
    }
}
