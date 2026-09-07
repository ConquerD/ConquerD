use std::env;
use std::path::PathBuf;

fn main() {
    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let opus_src = manifest_dir.join("opus");

    // ── Submodule check ────────────────────────────────────────────────────
    if !opus_src.join("CMakeLists.txt").exists() {
        panic!(
            "\n\
             libopus source not found at `rust/conquerd-opus/opus/`.\n\
             Initialize the git submodule before building:\n\
             \n\
             \tgit submodule update --init rust/conquerd-opus/opus\n\
             \n\
             The submodule tracks https://github.com/xiph/opus at the v1.5.2 tag.\n"
        );
    }

    // ── DNN data-file check (only when `dnn` feature is active) ──────────
    //
    // The DNN model weights ship as C source arrays (not a binary blob).
    // They are NOT in the xiph/opus git repo; they must be extracted from the
    // opus_data tarball before building.  `lace_data.c` is used as the
    // sentinel because it is always present when the tarball has been extracted.
    //
    // Download and extract with:
    //   powershell scripts/fetch_opus_weights.ps1   (Windows)
    //   bash scripts/fetch_opus_weights.sh           (Linux/macOS)
    //
    // The tarball URL encodes its own SHA-256 in the filename, so the download
    // is self-verifying.
    let dnn_enabled = env::var("CARGO_FEATURE_DNN").is_ok();
    if dnn_enabled {
        let sentinel_c = opus_src.join("dnn").join("lace_data.c");
        let sentinel_h = opus_src.join("dnn").join("fargan_data.h");
        if !sentinel_c.exists() || !sentinel_h.exists() {
            panic!(
                "\n\
                 DNN model data files not found in `rust/conquerd-opus/opus/dnn/`.\n\
                 \n\
                 Extract them with one of:\n\
                 \tpowershell scripts/fetch_opus_weights.ps1   (Windows)\n\
                 \tbash scripts/fetch_opus_weights.sh           (Linux/macOS)\n\
                 \n\
                 Or build without neural features by disabling the default `dnn` feature:\n\
                 \tconquerd-opus = {{ path = \"../conquerd-opus\", default-features = false }}\n"
            );
        }
        println!("cargo:rerun-if-changed=opus/dnn/lace_data.c");
        println!("cargo:rerun-if-changed=opus/dnn/fargan_data.h");
        println!("cargo:rerun-if-changed=opus/dnn/nolace_data.c");
    }

    // ── Build libopus as a static library via cmake ────────────────────────
    //
    // Key flags:
    //   BUILD_SHARED_LIBS=OFF          — static lib only
    //   OPUS_BUILD_TESTING=OFF         — skip opus's own test binaries
    //   OPUS_DRED=ON                   — compile DRED encoder/decoder support
    //   FETCHCONTENT_FULLY_DISCONNECTED=TRUE — prevent cmake from trying to
    //       download the DNN model data files at configure time; the C source
    //       arrays must already be present in opus/dnn/ (extracted by the
    //       fetch_opus_weights script) before cmake runs.
    //
    // On Windows MSVC prefer the Ninja generator so cached `target/` trees
    // survive GitHub runner Visual Studio upgrades (VS 17 → VS 18, etc.).
    // A stale Visual Studio CMakeCache in OUT_DIR would otherwise fail configure.
    // Build scripts run on the *host*, so `cfg!(target_os = ...)` here would
    // describe this machine rather than what is being built for. Cargo passes
    // the real target through the environment instead.
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();

    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    let cmake_build_dir = out_dir.join("build");
    let use_ninja = should_use_ninja_generator();
    if use_ninja {
        clear_stale_visual_studio_cmake_cache(&cmake_build_dir);
    }

    let mut config = cmake::Config::new(&opus_src);
    config
        .define("BUILD_SHARED_LIBS", "OFF")
        .define("OPUS_BUILD_SHARED_LIBRARY", "OFF")
        .define("OPUS_BUILD_TESTING", "OFF")
        .define("OPUS_DRED", "ON")
        .define("FETCHCONTENT_FULLY_DISCONNECTED", "TRUE")
        .profile("Release");
    if use_ninja {
        config.generator("Ninja");
    }

    // Android needs the NDK's own cmake toolchain file. Without it cmake's
    // Android-Determine module aborts with "Neither the NDK or a standalone
    // toolchain was found" — the `--target=` flags cargo-ndk puts in CFLAGS
    // tell the *compiler* what to emit but never tell cmake where the compiler
    // lives.
    if target_os == "android" {
        let ndk = android_ndk_root().unwrap_or_else(|| {
            panic!(
                "
                 Android NDK not found, but the build target is Android.
                 Set one of ANDROID_NDK_HOME / ANDROID_NDK_ROOT / NDK_HOME to the
                 NDK root, or ANDROID_HOME to an SDK containing `ndk/<version>/`.
                 
                 	sdkmanager \"ndk;28.2.13676358\"
"
            )
        });
        config.define(
            "CMAKE_TOOLCHAIN_FILE",
            ndk.join("build")
                .join("cmake")
                .join("android.toolchain.cmake"),
        );
        config.define("ANDROID_ABI", android_abi(&target_arch));
        config.define(
            "ANDROID_PLATFORM",
            format!("android-{}", android_api_level()),
        );
    }

    let dst = config.build();

    // The cmake output layout varies by generator and platform:
    //   Unix Makefiles / Ninja:  <dst>/lib/libopus.a
    //   MSVC Visual Studio:      <dst>/lib/Release/opus.lib
    println!("cargo:rustc-link-search=native={}/lib", dst.display());
    println!(
        "cargo:rustc-link-search=native={}/lib/Release",
        dst.display()
    );
    println!("cargo:rustc-link-lib=static=opus");

    // On Linux/Android, libopus may depend on -lm.
    if target_os == "linux" || target_os == "android" {
        println!("cargo:rustc-link-lib=m");
    }

    // ── Compile C shim ─────────────────────────────────────────────────────
    //
    // The shim wraps all variadic opus_*_ctl() calls with fixed C signatures,
    // avoiding variadic-FFI complexity in the Rust layer.
    cc::Build::new()
        .file("src/shim.c")
        .include(opus_src.join("include"))
        .opt_level(2)
        .warnings(false) // opus headers generate warnings on some compilers
        .compile("conquerd_opus_shim");

    println!("cargo:rerun-if-changed=src/shim.c");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=opus/include/opus.h");
    println!("cargo:rerun-if-changed=opus/include/opus_defines.h");
    println!("cargo:rerun-if-changed=opus/CMakeLists.txt");
    println!("cargo:rerun-if-env-changed=CMAKE_GENERATOR");
}

/// Use Ninja on Windows MSVC when available — avoids coupling the cmake cache
/// to a specific Visual Studio generator version on CI runners.
fn should_use_ninja_generator() -> bool {
    #[cfg(all(target_os = "windows", target_env = "msvc"))]
    {
        std::process::Command::new("ninja")
            .arg("--version")
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }
    #[cfg(not(all(target_os = "windows", target_env = "msvc")))]
    {
        false
    }
}

/// Drop a cached Visual Studio cmake tree so we can reconfigure with Ninja.
fn clear_stale_visual_studio_cmake_cache(cmake_build_dir: &std::path::Path) {
    let cache = cmake_build_dir.join("CMakeCache.txt");
    if !cache.exists() {
        return;
    }
    let Ok(text) = std::fs::read_to_string(&cache) else {
        return;
    };
    if text.contains("Visual Studio") {
        let _ = std::fs::remove_dir_all(cmake_build_dir);
    }
}

// ---------------------------------------------------------------------------
// Android cross-compilation helpers
// ---------------------------------------------------------------------------

/// Locate the Android NDK root — the directory holding `build/cmake/android.toolchain.cmake`.
///
/// Checks the conventional environment variables first (what `cargo-ndk` and
/// Gradle both set), then falls back to scanning `$ANDROID_HOME/ndk/` and
/// taking the highest version present.
fn android_ndk_root() -> Option<PathBuf> {
    let has_toolchain = |p: &PathBuf| {
        p.join("build")
            .join("cmake")
            .join("android.toolchain.cmake")
            .exists()
    };

    for key in [
        "ANDROID_NDK_HOME",
        "ANDROID_NDK_ROOT",
        "NDK_HOME",
        "NDK_ROOT",
    ] {
        println!("cargo:rerun-if-env-changed={key}");
        if let Ok(raw) = env::var(key) {
            let path = PathBuf::from(raw);
            if has_toolchain(&path) {
                return Some(path);
            }
        }
    }

    for key in ["ANDROID_HOME", "ANDROID_SDK_ROOT"] {
        println!("cargo:rerun-if-env-changed={key}");
        let Ok(sdk) = env::var(key) else { continue };
        let ndk_dir = PathBuf::from(sdk).join("ndk");
        let Ok(entries) = std::fs::read_dir(&ndk_dir) else {
            continue;
        };

        // Sort by numeric version components, not lexically: "9.0.x" must not
        // outrank "28.2.x" the way a plain string compare would have it.
        let mut candidates: Vec<(Vec<u64>, PathBuf)> = entries
            .flatten()
            .map(|e| e.path())
            .filter(has_toolchain)
            .map(|p| {
                let key = p
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or_default()
                    .split('.')
                    .map(|part| part.parse::<u64>().unwrap_or(0))
                    .collect::<Vec<_>>();
                (key, p)
            })
            .collect();
        candidates.sort();
        if let Some((_, path)) = candidates.pop() {
            return Some(path);
        }
    }

    None
}

/// Map a Rust target architecture onto the matching Android ABI directory name.
fn android_abi(target_arch: &str) -> &'static str {
    match target_arch {
        "aarch64" => "arm64-v8a",
        "arm" => "armeabi-v7a",
        "x86_64" => "x86_64",
        "x86" => "x86",
        other => panic!("unsupported Android target architecture: {other}"),
    }
}

/// Minimum Android API level to compile libopus against.
///
/// `cargo-ndk` already encodes the level it was invoked with into the target
/// triple it puts in `CFLAGS_<triple>` (e.g. `--target=aarch64-linux-android26`),
/// so parse that rather than letting cmake and cargo disagree about the level.
fn android_api_level() -> u32 {
    const DEFAULT_API: u32 = 26;

    for key in [
        "CARGO_NDK_ANDROID_PLATFORM",
        "ANDROID_PLATFORM",
        "ANDROID_API_LEVEL",
    ] {
        println!("cargo:rerun-if-env-changed={key}");
        if let Ok(raw) = env::var(key) {
            // Accepts "26" and "android-26" alike.
            let digits = raw.trim().trim_start_matches("android-");
            if let Ok(level) = digits.parse::<u32>() {
                return level;
            }
        }
    }

    // Fall back to whatever level the C flags were already built against.
    if let Ok(target) = env::var("TARGET") {
        let cflags_key = format!("CFLAGS_{}", target.replace('-', "_"));
        println!("cargo:rerun-if-env-changed={cflags_key}");
        if let Ok(cflags) = env::var(&cflags_key) {
            if let Some(level) = parse_api_from_cflags(&cflags) {
                return level;
            }
        }
    }

    DEFAULT_API
}

/// Pull the trailing API level out of a `--target=<triple><level>` clang flag.
///
/// Cargo does not compile build scripts under `cfg(test)`, so this cannot be
/// unit-tested from here. It is written to fail safe instead: anything it
/// cannot parse yields `None` and the caller falls back to `DEFAULT_API`.
fn parse_api_from_cflags(cflags: &str) -> Option<u32> {
    cflags.split_whitespace().find_map(|flag| {
        let triple = flag.strip_prefix("--target=")?;
        let digits: String = triple
            .chars()
            .rev()
            .take_while(char::is_ascii_digit)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        digits.parse().ok()
    })
}
