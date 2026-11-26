//! `cargo xtask ci`: the commands the CI workflow runs, on this machine.
//!
//! On Windows these are the `check` job's (the whole workspace). Elsewhere
//! they are the `portable` job's, then a type-check of the Windows build
//! (`--target x86_64-pc-windows-msvc`, no linking), so a Windows-only
//! clippy error is found before a push rather than by CI.

use std::path::Path;
use std::process::Command;

use anyhow::{Result, bail};

/// The crates the `portable` job checks (its `PORTABLE` list).
pub const PORTABLE: &[&str] = &[
    "neonshim-core",
    "neonshim-pe",
    "neonshim-pipeline",
    "neonshim-shader",
    "neonshim-formats",
    "neonshim-game",
    "xtask",
];

const WINDOWS: &str = "x86_64-pc-windows-msvc";

struct Step {
    what: &'static str,
    args: Vec<String>,
    env: Vec<(&'static str, &'static str)>,
}

fn step(what: &'static str, args: &[&str]) -> Step {
    Step {
        what,
        args: args.iter().map(|a| a.to_string()).collect(),
        env: vec![],
    }
}

/// What `cargo xtask ci` runs here, in order.
fn steps(windows_host: bool, windows_check: bool) -> Vec<Step> {
    let clippy = ["clippy", "--all-targets", "--locked"];
    let deny = ["--", "-D", "warnings"];
    let cat = |parts: &[&[&str]]| -> Vec<String> {
        parts
            .iter()
            .flat_map(|p| p.iter().map(|a| a.to_string()))
            .collect()
    };
    let portable: Vec<&str> = PORTABLE.iter().flat_map(|p| ["-p", p]).collect();
    if windows_host {
        return vec![
            Step {
                what: "clippy (workspace)",
                args: cat(&[&clippy, &["--workspace"], &deny]),
                env: vec![],
            },
            Step {
                what: "clippy (workspace, all features)",
                args: cat(&[&clippy, &["--workspace", "--all-features"], &deny]),
                env: vec![],
            },
            step("tests (workspace)", &["test", "--workspace", "--locked"]),
        ];
    }
    let mut steps = vec![
        Step {
            what: "clippy (portable crates)",
            args: cat(&[&clippy, &portable, &deny]),
            env: vec![],
        },
        Step {
            what: "tests (portable crates)",
            args: cat(&[&["test", "--locked"], &portable]),
            env: vec![],
        },
    ];
    if windows_check {
        // blake3's build script compiles C and assembly for the target,
        // which needs MSVC's tools; its portable Rust code type-checks the
        // same callers.
        let pure = vec![("CARGO_FEATURE_PURE", "1")];
        steps.push(Step {
            what: "clippy (workspace, Windows target, no linking)",
            args: cat(&[&clippy, &["--workspace", "--target", WINDOWS], &deny]),
            env: pure.clone(),
        });
        steps.push(Step {
            what: "clippy (workspace, Windows target, all features)",
            args: cat(&[
                &clippy,
                &["--workspace", "--all-features", "--target", WINDOWS],
                &deny,
            ]),
            env: pure,
        });
    }
    steps
}

pub fn run(root: &Path, windows_check: bool) -> Result<()> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    for s in steps(cfg!(windows), windows_check) {
        println!("== {}: cargo {}", s.what, s.args.join(" "));
        let status = Command::new(&cargo)
            .args(&s.args)
            .envs(s.env.iter().copied())
            .current_dir(root)
            .status()?;
        if !status.success() {
            bail!("{} failed ({status})", s.what);
        }
    }
    println!("== all CI steps passed");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `PORTABLE` list in the workflow, in order.
    fn workflow_portable() -> Vec<String> {
        let yml = include_str!("../../.github/workflows/ci.yml");
        let at = yml.find("PORTABLE: >-").expect("ci.yml sets PORTABLE");
        yml[at..]
            .lines()
            .skip(1)
            .take_while(|l| l.trim_start().starts_with("-p "))
            .flat_map(|l| {
                l.split_whitespace()
                    .filter(|w| *w != "-p")
                    .map(String::from)
            })
            .collect()
    }

    #[test]
    fn portable_list_matches_the_workflow() {
        assert_eq!(workflow_portable(), PORTABLE);
    }

    #[test]
    fn steps_follow_the_jobs() {
        let args = |windows_host, windows_check| -> Vec<String> {
            steps(windows_host, windows_check)
                .iter()
                .map(|s| s.args.join(" "))
                .collect()
        };
        let windows = args(true, true);
        assert_eq!(windows.len(), 3);
        assert!(windows[1].contains("--all-features"));
        let linux = args(false, false);
        assert_eq!(linux.len(), 2);
        assert!(linux[0].starts_with("clippy --all-targets --locked -p neonshim-core"));
        assert!(linux[1].starts_with("test --locked -p neonshim-core"));
        let cross = steps(false, true);
        assert_eq!(cross.len(), 4);
        assert!(
            cross[2]
                .args
                .join(" ")
                .contains("--target x86_64-pc-windows-msvc")
        );
        assert_eq!(cross[2].env, [("CARGO_FEATURE_PURE", "1")]);
    }
}
