//! Builds VP8 out of the vendored libvpx tree.
//!
//! # Why not libvpx's own build system
//!
//! libvpx ships a `configure` shell script plus GNU make, and on MSVC it wants
//! yasm or nasm for its hand-written assembly. That is three build-time tools
//! (`bash`, `make`, an assembler) that would have to exist on every developer
//! machine and on all four CI targets — win64, linux-x86_64, linux-aarch64, and
//! macOS. `conquerd-opus` gets away with vendoring because libopus builds with
//! CMake, which libvpx does not.
//!
//! So this compiles libvpx's C sources directly with the `cc` crate, supplying
//! the two things `configure` would otherwise generate: `vpx_config.h` (and its
//! make-syntax twin for the RTCD generator) and the RTCD headers themselves,
//! which come from libvpx's own `build/make/rtcd.pl`. Perl is the one tool this
//! needs beyond a C compiler, and it is already present on all three platforms
//! (Git for Windows ships it; Linux and macOS have it in base).
//!
//! # The cost: no SIMD
//!
//! Configuring for a generic architecture drops libvpx's x86/NEON assembly, so
//! encode and decode run pure C. That is a real cost — several times slower
//! than the SIMD paths — accepted because the alternative is having no codec at
//! all on Linux, and because the call default is 640x360 at 30 fps rather than
//! 1080p. Re-enabling SIMD later means adding an assembler to the build and
//! turning on the matching `VPX_ARCH_*` / `HAVE_*` flags in `ConfigFlags`;
//! nothing above this crate changes, since the RTCD indirection is exactly the
//! seam libvpx uses to select implementations.

use std::path::{Path, PathBuf};

/// libvpx's own source manifests, paired with the directory their entries are
/// relative to.
///
/// The build reads these rather than globbing directories, because which files
/// belong in a given configuration is a question libvpx already answers here —
/// and answers with `ifeq` blocks, not just per-line flags. `vpx_dsp/`, for
/// instance, holds VP9-only sources (`vpx_convolve.c`, `loopfilter.c`) inside
/// an `ifeq ($(CONFIG_VP9),yes)` block; a directory glob picks them up and they
/// fail to compile against a VP8-only RTCD header. Parsing keeps the source
/// list correct across submodule updates instead of rotting into a hand-list.
const SRC_MAKEFILES: &[(&str, &str)] = &[
    ("vpx/vpx_codec.mk", "vpx"),
    ("vp8/vp8_common.mk", "vp8"),
    ("vp8/vp8cx.mk", "vp8"),
    ("vp8/vp8dx.mk", "vp8"),
    ("vpx_dsp/vpx_dsp.mk", "vpx_dsp"),
    ("vpx_mem/vpx_mem.mk", "vpx_mem"),
    ("vpx_scale/vpx_scale.mk", "vpx_scale"),
    ("vpx_util/vpx_util.mk", "vpx_util"),
];

/// Evaluate one libvpx `.mk` manifest against `flags`, returning the `.c`
/// sources it selects (paths relative to the libvpx tree root).
///
/// This understands the subset of make syntax those manifests actually use:
///
/// * `SRCS-yes += file.c` — unconditional.
/// * `SRCS-$(CONFIG_X) += file.c` — included when the flag is on. Several
///   `$(...)` may be concatenated (`$(VPX_ARCH_X86)$(VPX_ARCH_X86_64)`), in
///   which case every one must be on, matching make's textual expansion.
/// * `ifeq ($(FLAG),yes)` / `ifneq ($(filter yes,$(A) $(B)),)` / `else` /
///   `endif` — nested, tracked with a stack.
///
/// Anything unrecognised is treated as disabled: a source this build does not
/// understand the condition for is better left out (a missing optional file
/// surfaces as a link error naming the symbol) than let in (which fails as a
/// wall of errors inside vendored C).
fn sources_from_makefile(text: &str, base: &str, flags: &Flags) -> Vec<String> {
    let mut out = Vec::new();
    // Each entry: (this branch is active, any branch of this if has been taken)
    let mut stack: Vec<(bool, bool)> = Vec::new();
    let active = |stack: &[(bool, bool)]| stack.iter().all(|(a, _)| *a);

    for raw in text.lines() {
        let line = raw.trim();

        if let Some(cond) = line.strip_prefix("ifeq ") {
            let taken = eval_ifeq(cond, flags);
            stack.push((taken, taken));
            continue;
        }
        if let Some(cond) = line.strip_prefix("ifneq ") {
            let taken = !eval_ifeq(cond, flags);
            stack.push((taken, taken));
            continue;
        }
        if line == "else" {
            if let Some((a, ever)) = stack.pop() {
                let _ = a;
                stack.push((!ever, true));
            }
            continue;
        }
        if line == "endif" || line.starts_with("endif ") || line.starts_with("endif\t") {
            stack.pop();
            continue;
        }
        if !active(&stack) {
            continue;
        }

        // `<VAR>-<cond> += <path>`
        let Some((lhs, rhs)) = line.split_once("+=") else {
            continue;
        };
        let path = rhs.trim();
        if !path.ends_with(".c") {
            continue; // headers, .mk, .asm
        }
        let Some((_, cond)) = lhs.trim().rsplit_once('-') else {
            continue;
        };
        if !cond_enabled(cond.trim(), flags) {
            continue;
        }
        out.push(format!("{base}/{path}"));
    }
    out
}

/// Is a `SRCS-<cond>` condition satisfied? `yes`, or every `$(FLAG)` on.
fn cond_enabled(cond: &str, flags: &Flags) -> bool {
    if cond == "yes" {
        return true;
    }
    let mut rest = cond;
    let mut saw_one = false;
    while let Some(start) = rest.find("$(") {
        let Some(end) = rest[start..].find(')') else {
            return false;
        };
        let name = &rest[start + 2..start + end];
        if !flags.is_on(name) {
            return false;
        }
        saw_one = true;
        rest = &rest[start + end + 1..];
    }
    saw_one
}

/// Evaluate an `ifeq (...)` argument. Handles the two forms libvpx uses:
/// `($(FLAG),yes)` and `($(filter yes,$(A) $(B)),)`.
fn eval_ifeq(cond: &str, flags: &Flags) -> bool {
    let inner = cond.trim().trim_start_matches('(').trim_end_matches(')');
    if let Some(filter) = inner.strip_prefix("$(filter yes,") {
        // `$(filter yes,$(A) $(B)),` compared against empty: true when none
        // are yes, so `ifneq` (the only way libvpx writes it) means "any".
        let names = filter.split(')').next().unwrap_or_default();
        let any = names
            .split_whitespace()
            .filter_map(|t| t.trim().strip_prefix("$("))
            .any(|t| flags.is_on(t.trim_end_matches(')')));
        return !any;
    }
    let Some((lhs, rhs)) = inner.split_once(',') else {
        return false;
    };
    let want_yes = rhs.trim() == "yes";
    let name = lhs.trim().trim_start_matches("$(").trim_end_matches(')');
    flags.is_on(name) == want_yes
}

fn main() {
    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let vpx = manifest_dir.join("libvpx");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());

    if !vpx.join("configure").exists() {
        panic!(
            "\n\
             libvpx source not found at `rust/conquerd-vpx/libvpx/`.\n\
             Initialize the git submodule before building:\n\
             \n\
             \tgit submodule update --init rust/conquerd-vpx/libvpx\n\
             \n\
             The submodule tracks https://github.com/webmproject/libvpx.\n"
        );
    }

    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let unix = target_os != "windows";

    // ── Generated configuration ─────────────────────────────────────────────
    let cfg = ConfigFlags::for_target(unix);
    let gen = out.join("vpx-generated");
    std::fs::create_dir_all(&gen).expect("create generated dir");
    std::fs::write(gen.join("vpx_config.h"), cfg.as_c_header()).expect("write vpx_config.h");
    std::fs::write(gen.join("vpx_config.mk"), cfg.as_make_fragment()).expect("write vpx_config.mk");
    std::fs::write(gen.join("vpx_version.h"), version_header()).expect("write vpx_version.h");
    // `vpx_codec_build_config()` returns this string; the real build derives it
    // from configure's argv. Ours is fixed, so a literal is honest.
    std::fs::write(
        gen.join("vpx_config.c"),
        "static const char* const cfg = \"generic-c-vp8-only\";\n\
         const char *vpx_codec_build_config(void) { return cfg; }\n",
    )
    .expect("write vpx_config.c");

    // ── RTCD headers, via libvpx's own generator ────────────────────────────
    for (sym, defs) in [
        ("vp8_rtcd", "vp8/common/rtcd_defs.pl"),
        ("vpx_scale_rtcd", "vpx_scale/vpx_scale_rtcd.pl"),
        ("vpx_dsp_rtcd", "vpx_dsp/vpx_dsp_rtcd_defs.pl"),
    ] {
        run_rtcd(&vpx, &gen, sym, defs);
    }

    // ── Compile ─────────────────────────────────────────────────────────────
    let mut build = cc::Build::new();
    build
        .include(&gen)
        .include(&vpx)
        // libvpx includes its own headers as "vpx/vpx_encoder.h" etc., relative
        // to the tree root, which the line above covers.
        .file(gen.join("vpx_config.c"))
        .warnings(false);

    let mut sources: Vec<String> = Vec::new();
    for (mk, base) in SRC_MAKEFILES {
        let text =
            std::fs::read_to_string(vpx.join(mk)).unwrap_or_else(|e| panic!("read {mk}: {e}"));
        sources.extend(sources_from_makefile(&text, base, &cfg));
        println!("cargo:rerun-if-changed=libvpx/{mk}");
    }
    sources.sort();
    sources.dedup();

    // `vp8/common/generic/systemdependent.c` provides `vp8_machine_specific_config`
    // and is selected by an architecture condition none of which hold in a
    // generic build, so the manifests never name it. It is still required —
    // it is the generic-architecture implementation.
    let generic_sysdep = "vp8/common/generic/systemdependent.c".to_owned();
    if !sources.contains(&generic_sysdep) {
        sources.push(generic_sysdep);
    }

    let mut compiled = 0usize;
    for rel in &sources {
        let path = vpx.join(rel);
        if !path.exists() {
            // A manifest naming a file the checkout lacks means the submodule
            // and this build script disagree about the tree; say so rather than
            // failing later with an undefined symbol.
            panic!("libvpx manifest lists {rel}, which does not exist in the submodule");
        }
        build.file(&path);
        compiled += 1;
    }
    assert!(
        compiled > 50,
        "only {compiled} libvpx sources selected — the manifest parse is wrong"
    );

    if unix {
        // libvpx's threading uses pthreads directly.
        build.flag_if_supported("-pthread");
        println!("cargo:rustc-link-lib=pthread");
        // The vendored C is not warning-clean under our lint settings and is
        // not ours to fix.
        build.flag_if_supported("-Wno-unused-but-set-variable");
    }

    build.file(manifest_dir.join("src").join("shim.c")).include(
        // The shim includes <vpx/vpx_encoder.h>, so it needs the tree root too.
        &vpx,
    );

    build.compile("conquerd_vpx");

    println!("cargo:rerun-if-changed=src/shim.c");
    println!("cargo:rerun-if-changed=build.rs");
}

/// Locate a perl interpreter.
///
/// On Windows perl is normally present but *not* on the PATH cargo inherits:
/// Git for Windows ships it under its own `usr/bin`, which only lands on PATH
/// inside a Git Bash shell. Since the build must work from PowerShell, cmd, and
/// an IDE alike, the known install locations are probed rather than assuming a
/// developer has arranged their PATH.
fn find_perl() -> std::ffi::OsString {
    if let Some(p) = std::env::var_os("PERL") {
        return p;
    }
    // On PATH already (the normal case on Linux and macOS).
    if std::process::Command::new("perl")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        return "perl".into();
    }
    if cfg!(windows) {
        for candidate in [
            r"C:\Program Files\Git\usr\bin\perl.exe",
            r"C:\Program Files (x86)\Git\usr\bin\perl.exe",
            r"C:\Strawberry\perl\bin\perl.exe",
        ] {
            if Path::new(candidate).exists() {
                return candidate.into();
            }
        }
        // Git may be installed elsewhere; derive its root from git.exe.
        if let Ok(out) = std::process::Command::new("where").arg("git").output() {
            if let Some(first) = String::from_utf8_lossy(&out.stdout).lines().next() {
                // ...\Git\cmd\git.exe -> ...\Git\usr\bin\perl.exe
                if let Some(git_root) = Path::new(first.trim()).parent().and_then(|p| p.parent()) {
                    let p = git_root.join("usr").join("bin").join("perl.exe");
                    if p.exists() {
                        return p.into_os_string();
                    }
                }
            }
        }
    }
    "perl".into()
}

/// Run libvpx's `rtcd.pl` for one symbol set.
fn run_rtcd(vpx: &Path, gen: &Path, sym: &str, defs: &str) {
    let out_file = gen.join(format!("{sym}.h"));
    let output = std::process::Command::new(find_perl())
        .arg(vpx.join("build").join("make").join("rtcd.pl"))
        .arg("--arch=generic")
        .arg(format!("--sym={sym}"))
        .arg(format!("--config={}", gen.join("vpx_config.mk").display()))
        .arg(vpx.join(defs))
        .output()
        .unwrap_or_else(|e| {
            panic!(
                "failed to run perl for {sym}: {e}\n\n\
                 Building VP8 needs perl on PATH. It ships with Git for Windows, \
                 and is present in the base install on Linux and macOS."
            )
        });
    if !output.status.success() {
        panic!(
            "rtcd.pl failed for {sym}:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    std::fs::write(&out_file, output.stdout).unwrap_or_else(|e| panic!("write {sym}.h: {e}"));
}

fn version_header() -> String {
    // libvpx derives these from its git tag. Nothing in the VP8 paths branches
    // on them; they only surface through `vpx_codec_version_str()`.
    "#define VERSION_MAJOR 1\n\
     #define VERSION_MINOR 15\n\
     #define VERSION_PATCH 0\n\
     #define VERSION_EXTRA \"\"\n\
     #define VERSION_PACKED ((1<<16)|(15<<8)|(0))\n\
     #define VERSION_STRING_NOSP \"v1.15.0\"\n\
     #define VERSION_STRING \" v1.15.0\"\n"
        .to_owned()
}

/// The flags `configure` would otherwise write into `vpx_config.h`.
///
/// Held as an ordered list rather than a struct so the C header and the make
/// fragment are guaranteed to describe the same configuration — `rtcd.pl` reads
/// the latter to decide which implementations to emit, and a disagreement
/// between the two shows up as a link error against a function no source file
/// defines.
struct ConfigFlags(Vec<(&'static str, bool)>);

/// Shorthand for the flag table as the makefile parser sees it.
type Flags = ConfigFlags;

impl ConfigFlags {
    /// Is `name` enabled? Unknown names read as disabled, which is the safe
    /// direction: a source guarded by a flag this build has never heard of is
    /// one this build was not configured for.
    fn is_on(&self, name: &str) -> bool {
        self.0
            .iter()
            .find(|(k, _)| *k == name)
            .map(|(_, v)| *v)
            .unwrap_or(false)
    }
}

impl ConfigFlags {
    fn for_target(unix: bool) -> Self {
        let mut v: Vec<(&'static str, bool)> = Vec::new();

        // Architecture: generic. Every arch is off, which is what drops the
        // assembly and makes one source list work for x86_64 and aarch64 alike.
        for a in [
            "VPX_ARCH_ARM",
            "VPX_ARCH_AARCH64",
            "VPX_ARCH_MIPS",
            "VPX_ARCH_X86",
            "VPX_ARCH_X86_64",
            "VPX_ARCH_PPC",
            "VPX_ARCH_LOONGARCH",
        ] {
            v.push((a, false));
        }

        // Instruction-set features: all off, for the same reason.
        for h in [
            "HAVE_NEON",
            "HAVE_NEON_ASM",
            "HAVE_NEON_DOTPROD",
            "HAVE_NEON_I8MM",
            "HAVE_SVE",
            "HAVE_SVE2",
            "HAVE_MIPS32",
            "HAVE_DSPR2",
            "HAVE_MSA",
            "HAVE_MIPS64",
            "HAVE_MMX",
            "HAVE_SSE",
            "HAVE_SSE2",
            "HAVE_SSE3",
            "HAVE_SSSE3",
            "HAVE_SSE4_1",
            "HAVE_AVX",
            "HAVE_AVX2",
            "HAVE_AVX512",
            "HAVE_VSX",
            "HAVE_MMI",
            "HAVE_LSX",
            "HAVE_LASX",
            "HAVE_X86_ASM",
        ] {
            v.push((h, false));
        }

        v.push(("HAVE_VPX_PORTS", true));
        v.push(("HAVE_PTHREAD_H", unix));
        v.push(("HAVE_PTHREAD_SETNAME_NP", false));
        v.push(("HAVE_UNISTD_H", unix));

        // Codecs: VP8 only, both directions. VP9 more than doubles the source
        // list and nothing here speaks it.
        v.push(("CONFIG_VP8", true));
        v.push(("CONFIG_VP8_ENCODER", true));
        v.push(("CONFIG_VP8_DECODER", true));
        v.push(("CONFIG_VP9", false));
        v.push(("CONFIG_VP9_ENCODER", false));
        v.push(("CONFIG_VP9_DECODER", false));
        v.push(("CONFIG_ENCODERS", true));
        v.push(("CONFIG_DECODERS", true));

        v.push(("CONFIG_DEPENDENCY_TRACKING", false));
        v.push(("CONFIG_EXTERNAL_BUILD", false));
        v.push(("CONFIG_INSTALL_DOCS", false));
        v.push(("CONFIG_INSTALL_BINS", false));
        v.push(("CONFIG_INSTALL_LIBS", false));
        v.push(("CONFIG_INSTALL_SRCS", false));
        v.push(("CONFIG_DEBUG", false));
        v.push(("CONFIG_GPROF", false));
        v.push(("CONFIG_GCOV", false));
        v.push(("CONFIG_RVCT", false));
        v.push(("CONFIG_GCC", unix));
        v.push(("CONFIG_MSVS", !unix));
        // Position independence matters for the Linux/macOS shared-object case
        // and is harmless on Windows.
        v.push(("CONFIG_PIC", unix));
        // Every target this ships to is little-endian.
        v.push(("CONFIG_BIG_ENDIAN", false));
        v.push(("CONFIG_CODEC_SRCS", false));
        v.push(("CONFIG_DEBUG_LIBS", false));
        v.push(("CONFIG_DEQUANT_TOKENS", false));
        v.push(("CONFIG_DC_RECON", false));
        // No runtime dispatch: with one (C) implementation there is nothing to
        // dispatch between, and leaving it on would emit a resolver that
        // expects per-arch symbols we do not build.
        v.push(("CONFIG_RUNTIME_CPU_DETECT", false));
        v.push(("CONFIG_POSTPROC", false));
        v.push(("CONFIG_VP9_POSTPROC", false));
        v.push(("CONFIG_MULTITHREAD", true));
        v.push(("CONFIG_INTERNAL_STATS", false));
        v.push(("CONFIG_STATIC_MSVCRT", false));
        v.push(("CONFIG_SPATIAL_RESAMPLING", true));
        // Realtime-only drops the multi-pass / best-quality encode paths, which
        // a live call never uses.
        v.push(("CONFIG_REALTIME_ONLY", true));
        v.push(("CONFIG_ONTHEFLY_BITPACKING", false));
        v.push(("CONFIG_ERROR_CONCEALMENT", false));
        v.push(("CONFIG_SHARED", false));
        v.push(("CONFIG_STATIC", true));
        v.push(("CONFIG_SMALL", false));
        v.push(("CONFIG_POSTPROC_VISUALIZER", false));
        v.push(("CONFIG_OS_SUPPORT", true));
        v.push(("CONFIG_UNIT_TESTS", false));
        v.push(("CONFIG_WEBM_IO", false));
        v.push(("CONFIG_LIBYUV", false));
        v.push(("CONFIG_DECODE_PERF_TESTS", false));
        v.push(("CONFIG_ENCODE_PERF_TESTS", false));
        v.push(("CONFIG_MULTI_RES_ENCODING", false));
        v.push(("CONFIG_TEMPORAL_DENOISING", false));
        v.push(("CONFIG_VP9_TEMPORAL_DENOISING", false));
        v.push(("CONFIG_COEFFICIENT_RANGE_CHECKING", false));
        v.push(("CONFIG_VP9_HIGHBITDEPTH", false));
        v.push(("CONFIG_BETTER_HW_COMPATIBILITY", false));
        v.push(("CONFIG_EXPERIMENTAL", false));
        v.push(("CONFIG_SIZE_LIMIT", false));
        v.push(("CONFIG_ALWAYS_ADJUST_BPM", false));
        v.push(("CONFIG_BITSTREAM_DEBUG", false));
        v.push(("CONFIG_MISMATCH_DEBUG", false));
        v.push(("CONFIG_SPATIAL_SVC", false));
        v.push(("CONFIG_FP_MB_STATS", false));
        v.push(("CONFIG_EMULATE_HARDWARE", false));
        v.push(("CONFIG_NON_GREEDY_MV", false));
        v.push(("CONFIG_RATE_CTRL", false));
        v.push(("CONFIG_COLLECT_COMPONENT_TIMING", false));

        Self(v)
    }

    fn as_c_header(&self) -> String {
        let mut s = String::from(
            "/* Generated by conquerd-vpx/build.rs. Do not edit. */\n\
             #ifndef VPX_CONFIG_H\n#define VPX_CONFIG_H\n\
             #define RESTRICT\n\
             #define INLINE inline\n",
        );
        for (k, val) in &self.0 {
            s.push_str(&format!("#define {k} {}\n", if *val { 1 } else { 0 }));
        }
        s.push_str("#endif /* VPX_CONFIG_H */\n");
        s
    }

    /// The same flags in the make syntax `rtcd.pl` parses.
    fn as_make_fragment(&self) -> String {
        let mut s = String::new();
        for (k, val) in &self.0 {
            // rtcd.pl only looks at `yes`; anything else reads as disabled.
            s.push_str(&format!("{k}={}\n", if *val { "yes" } else { "no" }));
        }
        s
    }
}
