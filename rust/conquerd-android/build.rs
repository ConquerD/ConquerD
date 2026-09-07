//! Link configuration for the Android JNI cdylib.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    // Build scripts run on the host, so ask cargo what is actually being
    // built for rather than trusting `cfg!(target_os = ...)`.
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "android" {
        return;
    }

    // `oboe-sys` — reached through cpal's Android backend — is C++, and its
    // build script emits `-lc++_static`. That archive supplies libc++ but not
    // the ABI layer underneath it: `operator new`/`delete`, the `__cxxabiv1`
    // type-info vtables, `__cxa_pure_virtual`, `__gxx_personality_v0` and the
    // `std::runtime_error` family all live in `libc++abi.a`.
    //
    // libc++_static is the right choice here (exactly one .so in this APK uses
    // C++, which is the case Google recommends it for), so pair it rather than
    // switching to libc++_shared and shipping a second library.
    println!("cargo:rustc-link-lib=c++abi");

    // The reason the above was not caught at build time: a shared object is
    // allowed to have undefined symbols, so the link succeeded and produced a
    // .so that only failed at `dlopen` on the device — as a
    // "cannot locate symbol" crash with no build-time warning at all.
    //
    // Refuse that. Nothing here legitimately imports from the loader: JNI
    // entry points are *exported* by us, and everything the JVM provides
    // arrives through function pointers on `JNIEnv` rather than as linked
    // symbols. So an unresolved symbol is always a missing library.
    println!("cargo:rustc-link-arg=-Wl,--no-undefined");
}
