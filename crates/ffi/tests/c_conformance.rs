//! Header conformance: builds `libsimcore_ffi.a`, checks its exported symbols against
//! `include/simcore.h`, then compiles `tests/c/conformance.c` with clang against the header,
//! links the staticlib and runs it, normally and under AddressSanitizer.
//!
//! `cargo test` only builds the rlib, so the staticlib is built here with a nested
//! `cargo build` into its own target directory (sharing `target/` would deadlock on the build
//! lock held by the outer cargo).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn out_dir() -> PathBuf {
    root().join("target/c-conformance")
}

fn release() -> bool {
    !cfg!(debug_assertions)
}

/// Builds the staticlib once per test binary and returns its path.
fn staticlib() -> &'static Path {
    static LIB: OnceLock<PathBuf> = OnceLock::new();
    LIB.get_or_init(|| {
        let target_dir = out_dir().join("cargo");
        let mut cmd = Command::new(env!("CARGO"));
        cmd.current_dir(root())
            .args([
                "build",
                "--quiet",
                "-p",
                "simcore-ffi",
                "--lib",
                "--target-dir",
            ])
            .arg(&target_dir);
        if release() {
            cmd.arg("--release");
        }
        let status = cmd.status().expect("run cargo");
        assert!(status.success(), "cargo build of the staticlib failed");
        let lib = target_dir
            .join(if release() { "release" } else { "debug" })
            .join("libsimcore_ffi.a");
        assert!(lib.exists(), "missing {}", lib.display());
        lib
    })
}

/// Function names declared in the header: identifiers `sc_*` directly followed by `(`.
fn header_functions() -> Vec<String> {
    let header = std::fs::read_to_string(root().join("include/simcore.h")).unwrap();
    let mut names: Vec<String> = header
        .lines()
        .filter(|l| !l.trim_start().starts_with(['#', '/', '*']))
        .filter_map(|l| {
            let open = l.find("sc_")?;
            let rest = &l[open..];
            let end = rest.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))?;
            rest[end..]
                .starts_with('(')
                .then(|| rest[..end].to_string())
        })
        .collect();
    names.sort();
    names
}

#[test]
fn exported_symbols_match_header() {
    let expected = header_functions();
    assert_eq!(expected.len(), 19, "header functions: {expected:?}");

    // --no-llvm-bc: read the Mach-O symbol tables only. Without it Apple's nm also parses the
    // LLVM bitcode Rust embeds in its objects, fails ("Not an int attribute": newer LLVM
    // producer than Apple's reader) and exits non-zero. -g external, -U defined, -j names.
    let out = Command::new("nm")
        .args(["--no-llvm-bc", "-gUj"])
        .arg(staticlib())
        .output()
        .expect("run nm");
    assert!(
        out.status.success(),
        "nm failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let mut exported: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|s| s.strip_prefix("_sc_"))
        .map(|s| format!("sc_{s}"))
        .collect();
    exported.sort();
    exported.dedup();
    assert_eq!(
        exported, expected,
        "exported sc_* symbols differ from the header"
    );
}

fn compile_and_run(name: &str, extra: &[&str]) -> String {
    let exe = out_dir().join(name);
    let status = Command::new("clang")
        .args([
            "-std=c11",
            "-Wall",
            "-Wextra",
            "-Wpedantic",
            "-Werror",
            "-g",
            "-O1",
        ])
        .args(extra)
        .arg("-I")
        .arg(root().join("include"))
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/c/conformance.c"))
        .arg(staticlib())
        // `--print native-static-libs` asks for -lSystem -lc -lm; clang links libSystem
        // (which provides libc and libm) by default, and passing it again makes ld warn.
        .arg("-o")
        .arg(&exe)
        .status()
        .expect("run clang");
    assert!(status.success(), "clang failed for {name}");

    let out = Command::new(&exe)
        .env(
            "ASAN_OPTIONS",
            "halt_on_error=1:detect_stack_use_after_return=1",
        )
        .output()
        .expect("run conformance binary");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    // The binaries are kept in target/c-conformance; the transcript is useful when reporting.
    println!("{name} stdout:\n{stdout}\n{name} stderr:\n{stderr}");
    assert!(out.status.success(), "{name} failed:\n{stdout}\n{stderr}");
    assert!(
        !stderr.contains("AddressSanitizer"),
        "{name}: ASan report:\n{stderr}"
    );
    stdout
}

#[test]
fn c_program_conforms() {
    let stdout = compile_and_run("conformance", &[]);
    assert!(stdout.contains("conformance: ok"));
}

#[test]
fn c_program_conforms_under_asan() {
    let stdout = compile_and_run(
        "conformance-asan",
        &["-fsanitize=address", "-fno-omit-frame-pointer"],
    );
    assert!(stdout.contains("conformance: ok"));
}
