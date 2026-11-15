#[path = "src/build_info/validation.rs"]
mod validation;

use std::{path::Path, process::Command};

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8(output.stdout).ok())
        .flatten()
}

fn main() {
    for name in [
        "HERDR_BUILD_REVISION",
        "HERDR_BUILD_COMMIT_DATE",
        "HERDR_BUILD_DIRTY",
    ] {
        println!("cargo:rerun-if-env-changed={name}");
    }
    println!("cargo:rerun-if-changed=src/build_info/validation.rs");
    println!("cargo:rerun-if-changed=frontend/dist");
    if !Path::new("frontend/dist/index.html").exists() {
        panic!("Frontend assets missing. Run: cd frontend && npm ci && npm run build");
    }
    // Track refs, the index, and working-tree changes (including untracked files).
    // Ignored outputs such as target/ and frontend/dist/ never make a build dirty.
    for input in [
        "src",
        "frontend/src",
        "frontend/package.json",
        "frontend/package-lock.json",
        "Cargo.toml",
        "Cargo.lock",
        "build.rs",
    ] {
        println!("cargo:rerun-if-changed={input}");
    }
    // Track all existing tracked/untracked non-ignored inputs, not just the
    // index: unstaged edits must refresh a previously clean provenance stamp.
    if let Some(files) = git(&[
        "ls-files",
        "-z",
        "--cached",
        "--others",
        "--exclude-standard",
    ]) {
        for file in files
            .split('\0')
            .filter(|s| !s.is_empty() && !s.contains(['\n', '\r']))
        {
            println!("cargo:rerun-if-changed={file}");
        }
    }
    for name in ["HEAD", "index", "packed-refs"] {
        if let Some(path) = git(&["rev-parse", "--git-path", name]) {
            println!("cargo:rerun-if-changed={}", path.trim());
        }
    }
    if let Some(reference) = git(&["symbolic-ref", "-q", "HEAD"])
        && let Some(path) = git(&["rev-parse", "--git-path", reference.trim()])
    {
        println!("cargo:rerun-if-changed={}", path.trim());
    }
    let expected = std::env::var("HERDR_BUILD_REVISION")
        .ok()
        .filter(|v| validation::revision(v));
    let head = git(&["rev-parse", "HEAD"]);
    let matches = expected
        .as_deref()
        .zip(head.as_deref())
        .is_some_and(|(a, b)| a.eq_ignore_ascii_case(b.trim()));
    let date = if matches {
        git(&["show", "-s", "--format=%cI", expected.as_deref().unwrap()])
            .map(|s| s.trim().to_owned())
            .filter(|s| validation::commit_date(s))
    } else if expected.is_some() && head.is_none() {
        // Archive builders may explicitly attest a commit date. Never apply an
        // override to a synthetic/mismatching revision in an actual checkout.
        std::env::var("HERDR_BUILD_COMMIT_DATE")
            .ok()
            .filter(|s| validation::commit_date(s))
    } else {
        None
    };
    let dirty = if matches {
        git(&["status", "--porcelain=v1", "--untracked-files=all"]).map(|s| !s.is_empty())
    } else {
        None
    };
    // Empty exports mask inherited untrusted values, including HERDR_BUILD_DIRTY.
    println!(
        "cargo:rustc-env=HERDR_BUILD_COMMIT_DATE={}",
        date.as_deref().unwrap_or("")
    );
    println!(
        "cargo:rustc-env=HERDR_BUILD_DIRTY={}",
        dirty
            .map(|d| if d { "true" } else { "false" })
            .unwrap_or("")
    );
}
