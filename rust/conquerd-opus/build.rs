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
        let sentinel = opus_src.join("dnn").join("lace_data.c");
        if !sentinel.exists() {
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

    // On Linux/macOS, libopus may depend on -lm.
    #[cfg(target_os = "linux")]
    println!("cargo:rustc-link-lib=m");

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
        return std::process::Command::new("ninja")
            .arg("--version")
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
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
