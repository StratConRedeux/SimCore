//! Header conformance: builds `libsimcore_ffi.a`, checks its exported symbols against
//! `include/simcore.h`, then compiles `tests/c/conformance.c` with clang against the header,
//! links the staticlib and runs it, normally and under AddressSanitizer. A generated C
//! program also compares every struct layout and `SC_*` constant with the `abi` crate.
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

/// One per profile, so a debug and a release run never share binaries.
fn out_dir() -> PathBuf {
    root()
        .join("target/c-conformance")
        .join(if release() { "release" } else { "debug" })
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
    assert_eq!(expected.len(), 27, "header functions: {expected:?}");

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

// ---------------------------------------------------------------------------------------------
// Layout and constants, header against the `abi` crate, exhaustively.
//
// The C side is generated from the header: every `typedef struct X { ... } X;` with a body and
// every field declared in it, and every `static const T SC_* = ...;`. A generated C program
// prints each struct's size and alignment, each field's offset, size and type class, and each
// constant's size and value. The Rust side prints the same lines from `offset_of!`, `size_of`
// and the constants. The two transcripts must be identical, so a swapped, retyped, resized,
// missing or extra field (on either side) or a changed constant fails the test.
//
// Exhaustiveness on the Rust side: `rust_layout!` destructures each struct without `..`, so
// the field list below must name every Rust field (or the test does not compile), and the
// sets of structs and constants are compared with the `pub struct` / `pub const` items found
// in `crates/abi/src/lib.rs`.
// ---------------------------------------------------------------------------------------------

use simcore_abi as abi;
use std::collections::BTreeSet;

/// The C type class `_Generic` reports, and its Rust counterpart.
trait Class {
    const CLASS: &'static str;
}
macro_rules! class {
    ($($t:ty => $c:literal),* $(,)?) => { $(impl Class for $t { const CLASS: &'static str = $c; })* };
}
class!(u8 => "u8", u16 => "u16", u32 => "u32", u64 => "u64", f32 => "f32", f64 => "f64");
impl<T: Class> Class for *const T {
    const CLASS: &'static str = "ptr";
}

fn field_size<S, F>(_: fn(&S) -> &F) -> usize {
    std::mem::size_of::<F>()
}
fn field_class<S, F: Class>(_: fn(&S) -> &F) -> &'static str {
    F::CLASS
}

/// Rust field names map to C ones by dropping a trailing `_` (`type_` is `type`).
fn c_name(rust: &str) -> &str {
    rust.strip_suffix('_').unwrap_or(rust)
}

/// Expands to a `Class` impl per struct and `rust_layout_lines`, the struct part of the Rust
/// transcript.
macro_rules! rust_layout {
    ($($s:ident { $($f:ident),* $(,)? })*) => {
        $(impl Class for abi::$s {
            const CLASS: &'static str = stringify!($s);
        })*
        fn rust_layout_lines() -> Vec<String> {
            let mut out = Vec::new();
            $(
                // Compile-time exhaustiveness: no `..`, so every Rust field must be listed.
                let _ = |v: abi::$s| {
                    let abi::$s { $($f: _),* } = v;
                };
                out.push(format!(
                    "struct {} {} {}",
                    stringify!($s),
                    std::mem::size_of::<abi::$s>(),
                    std::mem::align_of::<abi::$s>()
                ));
                $(
                    out.push(format!(
                        "field {}.{} {} {} {}",
                        stringify!($s),
                        c_name(stringify!($f)),
                        std::mem::offset_of!(abi::$s, $f),
                        field_size(|s: &abi::$s| &s.$f),
                        field_class(|s: &abi::$s| &s.$f),
                    ));
                )*
            )*
            out
        }
    };
}

macro_rules! rust_consts {
    ($out:ident; $($c:ident = $($p:ident)::+),* $(,)?) => {$(
        $out.push(format!(
            "const {} {} {}",
            stringify!($c),
            std::mem::size_of_val(&abi::$($p)::+),
            u64::from(abi::$($p)::+)
        ));
    )*};
}

rust_layout! {
    ScUnitView { id, x, y, z, vx, vy, vz, kind, affiliation, flags, ems_power_w }
    ScFrame { units, count, _reserved, tick }
    ScEngagement {
        tick, pursuer_id, target_id, x, y, z, target_kind, target_affiliation, flags
    }
    ScEngagements { events, count, _reserved }
    ScCommand {
        unit_id, target_id, x, y, z, speed_mps, type_, radiate, _reserved0, _reserved1
    }
    ScStatus {
        tick, unit_count, link_delay_ticks, friendly_emitting, friendly_detected,
        hostile_detected, pending_commands, dropped_commands, command_node_hits,
        awaiting_cleanup, _reserved
    }
    ScBytes { data, len }
    ScRules {
        detection_max_range_m, sensor_base_range_m, sensor_reference_power_w,
        default_chance, exposure_threshold_ticks, exposure_cap_ticks,
        exposure_decay_per_tick, _reserved
    }
    ScOutcome { outcome_class, target_kind, _reserved, chance }
    ScLinkRules {
        delay_ticks_per_km, jam_radius_m, base_delay_ticks, jam_delay_ticks,
        max_delay_ticks, _reserved
    }
    ScTerrain { origin_x, origin_y, spacing_m, nx, ny, los_samples, _reserved }
    ScUnitSpec {
        x, y, z, vx, vy, vz, erp_w, kind, affiliation, options, cycle_on_ticks,
        cycle_off_ticks, cycle_phase, outcome_class, _reserved, target_id, speed_mps,
        max_turn_rate_rad_s, lead, engagement_radius_m
    }
    ScSensor { base_range_m, reference_power_w }
    ScStandingOrder {
        x, y, radius_m, kinds, stock, cooldown_ticks, _reserved, launch
    }
    ScLaunch { tick, order_id, pursuer_id, target_id }
    ScLaunches { launches, count, _reserved }
    ScStandingOrderView {
        id, x, y, radius_m, kinds, remaining, stock, ready_tick, cooldown_ticks,
        _reserved
    }
    ScStandingOrders { orders, count, _reserved }
}

/// The Rust transcript, in no particular order (it is sorted before comparing).
fn rust_transcript() -> Vec<String> {
    let mut out = rust_layout_lines();
    rust_consts! { out;
        SC_ABI_VERSION = ABI_VERSION,
        SC_TICKS_PER_SECOND = TICKS_PER_SECOND,
        SC_MAX_STANDING_ORDERS = MAX_STANDING_ORDERS,
        SC_AFFILIATION_UNKNOWN = affiliation::UNKNOWN,
        SC_AFFILIATION_FRIENDLY = affiliation::FRIENDLY,
        SC_AFFILIATION_HOSTILE = affiliation::HOSTILE,
        SC_AFFILIATION_NEUTRAL = affiliation::NEUTRAL,
        SC_KIND_UNKNOWN = kind::UNKNOWN,
        SC_KIND_COMMAND_NODE = kind::COMMAND_NODE,
        SC_KIND_QUADCOPTER = kind::QUADCOPTER,
        SC_KIND_FIXED_WING_DRONE = kind::FIXED_WING_DRONE,
        SC_KIND_GROUND_RADAR = kind::GROUND_RADAR,
        SC_KIND_GROUND_VEHICLE = kind::GROUND_VEHICLE,
        SC_KIND_JAMMER = kind::JAMMER,
        SC_FLAG_EMITTING = flags::EMITTING,
        SC_FLAG_COMMAND_NODE = flags::COMMAND_NODE,
        SC_FLAG_DETECTED = flags::DETECTED,
        SC_ENGAGEMENT_SUCCESS = engagement::SUCCESS,
        SC_ENGAGEMENT_TARGET_REMOVED = engagement::TARGET_REMOVED,
        SC_ENGAGEMENT_COMMAND_NODE_HIT = engagement::COMMAND_NODE_HIT,
        SC_COMMAND_MOVE = command::MOVE,
        SC_COMMAND_HALT = command::HALT,
        SC_COMMAND_RETARGET = command::RETARGET,
        SC_COMMAND_EMISSIONS = command::EMISSIONS,
        SC_SPAWN_EMITTING = spawn::EMITTING,
        SC_SPAWN_CYCLE = spawn::CYCLE,
        SC_SPAWN_PURSUER = spawn::PURSUER,
        SC_SPAWN_SENSOR = spawn::SENSOR,
    }
    out
}

/// Removes `/* ... */` comments.
fn strip_comments(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(start) = rest.find("/*") {
        out.push_str(&rest[..start]);
        let end = rest[start..].find("*/").expect("unterminated comment") + start + 2;
        out.push(' ');
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

fn is_ident(s: &str) -> bool {
    !s.is_empty()
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !s.starts_with(|c: char| c.is_ascii_digit())
}

/// Structs with bodies, in header order, each with its fields in declaration order.
fn header_structs(header: &str) -> Vec<(String, Vec<String>)> {
    let mut structs = Vec::new();
    let mut rest = header;
    while let Some(at) = rest.find("typedef struct ") {
        rest = &rest[at + "typedef struct ".len()..];
        let name_end = rest.find(|c: char| !is_ident(&c.to_string())).unwrap();
        let name = rest[..name_end].to_string();
        let after = rest[name_end..].trim_start();
        if !after.starts_with('{') {
            continue; // opaque (ScWorld)
        }
        let close = after.find('}').expect("struct body");
        let body = &after[1..close];
        assert!(!body.contains('{'), "nested struct body in {name}");
        let tail = after[close + 1..].trim_start();
        assert!(
            tail.starts_with(&format!("{name};")),
            "typedef name of {name}"
        );
        let mut fields = Vec::new();
        for decl in body.split(';').map(str::trim).filter(|d| !d.is_empty()) {
            assert!(!decl.contains('['), "array field in {name}: {decl}");
            assert!(!decl.contains(':'), "bit-field in {name}: {decl}");
            let mut parts = decl.split(',');
            // The first declarator's name is the last token of the first part.
            let first = parts.next().unwrap();
            let f = first.rsplit([' ', '*']).next().unwrap().trim();
            assert!(is_ident(f), "field name in {name}: {decl}");
            fields.push(f.to_string());
            for d in parts {
                let d = d.trim().trim_start_matches('*').trim();
                assert!(is_ident(d), "field name in {name}: {decl}");
                fields.push(d.to_string());
            }
        }
        structs.push((name, fields));
    }
    structs
}

/// `static const T SC_*` constant names, in header order.
fn header_constants(header: &str) -> Vec<String> {
    header
        .lines()
        .filter_map(|l| l.trim().strip_prefix("static const "))
        .map(|l| {
            let lhs = l.split('=').next().expect("initialiser").trim();
            let name = lhs.rsplit(' ').next().unwrap();
            assert!(name.starts_with("SC_") && is_ident(name), "{l}");
            name.to_string()
        })
        .collect()
}

/// A C program that prints the C transcript.
fn layout_program(structs: &[(String, Vec<String>)], consts: &[String]) -> String {
    let mut classes = String::from(
        "uint8_t: \"u8\", uint16_t: \"u16\", uint32_t: \"u32\", uint64_t: \"u64\", \
         float: \"f32\", double: \"f64\", const uint8_t *: \"ptr\"",
    );
    for (s, _) in structs {
        classes.push_str(&format!(", {s}: \"{s}\", const {s} *: \"ptr\""));
    }
    let mut c = format!(
        "#include \"simcore.h\"\n#include <stdalign.h>\n#include <stddef.h>\n#include <stdio.h>\n\
         #define CLASS(e) _Generic((e), {classes}, default: \"other\")\n"
    );
    for (s, _) in structs {
        c.push_str(&format!("{s} v_{s};\n")); // unevaluated operands only
    }
    c.push_str("int main(void) {\n");
    for (s, fields) in structs {
        c.push_str(&format!(
            "    printf(\"struct {s} %zu %zu\\n\", sizeof({s}), alignof({s}));\n"
        ));
        for f in fields {
            c.push_str(&format!(
                "    printf(\"field {s}.{f} %zu %zu %s\\n\", offsetof({s}, {f}), \
                 sizeof(v_{s}.{f}), CLASS(v_{s}.{f}));\n"
            ));
        }
    }
    for k in consts {
        c.push_str(&format!(
            "    printf(\"const {k} %zu %llu\\n\", sizeof({k}), (unsigned long long){k});\n"
        ));
    }
    c.push_str("    return 0;\n}\n");
    c
}

/// Names of `pub struct`s and of `pub const`s (as `SC_*` header names) in the abi crate.
fn abi_source_items() -> (BTreeSet<String>, BTreeSet<String>) {
    let src = std::fs::read_to_string(root().join("crates/abi/src/lib.rs")).unwrap();
    let prefix = |module: &str| match module {
        "" => "".to_string(),
        "flags" => "FLAG_".to_string(),
        m => format!("{}_", m.to_ascii_uppercase()),
    };
    let (mut structs, mut consts) = (BTreeSet::new(), BTreeSet::new());
    let mut module = String::new();
    for line in src.lines() {
        let t = line.trim();
        if let Some(m) = t.strip_prefix("pub mod ") {
            module = m.trim_end_matches(" {").to_string();
        } else if line == "}" {
            module.clear();
        } else if let Some(s) = t.strip_prefix("pub struct ") {
            structs.insert(s.trim_end_matches(" {").to_string());
        } else if let Some(k) = t.strip_prefix("pub const ") {
            let name = k.split(':').next().unwrap();
            consts.insert(format!("SC_{}{name}", prefix(&module)));
        }
    }
    (structs, consts)
}

#[test]
fn layout_and_constants_match_header() {
    let header =
        strip_comments(&std::fs::read_to_string(root().join("include/simcore.h")).unwrap());
    let structs = header_structs(&header);
    let consts = header_constants(&header);

    // The parse found what the header has (guards against a parser that silently skips).
    assert_eq!(structs.len(), 18, "header structs: {structs:?}");
    assert_eq!(consts.len(), 28, "header constants: {consts:?}");

    // The same items on both sides.
    let (abi_structs, abi_consts) = abi_source_items();
    let header_struct_names: BTreeSet<String> = structs.iter().map(|(s, _)| s.clone()).collect();
    assert_eq!(
        header_struct_names, abi_structs,
        "structs: header vs abi crate"
    );
    let header_const_names: BTreeSet<String> = consts.iter().cloned().collect();
    assert_eq!(
        header_const_names, abi_consts,
        "constants: header vs abi crate"
    );

    // Generate, compile and run the C side.
    std::fs::create_dir_all(out_dir()).unwrap();
    let src = out_dir().join("layout.c");
    let exe = out_dir().join("layout");
    std::fs::write(&src, layout_program(&structs, &consts)).unwrap();
    let status = Command::new("clang")
        .args(["-std=c11", "-Wall", "-Wextra", "-Wpedantic", "-Werror"])
        .arg("-I")
        .arg(root().join("include"))
        .arg(&src)
        .arg("-o")
        .arg(&exe)
        .status()
        .expect("run clang");
    assert!(status.success(), "clang failed for {}", src.display());
    let out = Command::new(&exe).output().expect("run layout program");
    assert!(out.status.success());
    let mut c_lines: Vec<String> = String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(String::from)
        .collect();
    assert!(
        !c_lines.iter().any(|l| l.ends_with(" other")),
        "a header field has a type the check does not classify:\n{}",
        c_lines.join("\n")
    );

    let mut rust_lines = rust_transcript();
    c_lines.sort();
    rust_lines.sort();
    let fields: usize = structs.iter().map(|(_, f)| f.len()).sum();
    assert_eq!(c_lines.len(), structs.len() + fields + consts.len());
    if c_lines != rust_lines {
        let c: BTreeSet<_> = c_lines.iter().collect();
        let r: BTreeSet<_> = rust_lines.iter().collect();
        panic!(
            "header and abi crate differ\nonly in the header (C):\n  {}\nonly in Rust:\n  {}",
            c.difference(&r)
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join("\n  "),
            r.difference(&c)
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join("\n  "),
        );
    }
}
