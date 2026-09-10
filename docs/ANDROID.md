# DoubleSlash on Android

The Android client runs the **same Rust core as the desktop client** — the same
transport, crypto, stores and feature registry — behind a native Kotlin/Compose
UI. Nothing about the trust model changes: identity is generated on the device,
keys never leave it, and there is still no first-party backend.

## Why not Qt/QML

The desktop UI is cxx-qt over QML. That was considered and rejected for Android:

* `cxx-qt-build` has no `androiddeployqt` integration, so APK packaging would be
  bespoke build-system work rather than configuration.
* The QML is desktop-shaped — a custom title bar, pop-out windows, hover states,
  right-click menus — so "reuse the UI" would have meant rewriting most of it
  regardless.
* Qt WebEngine, which backs the in-app portal, the browser panel and file
  preview, **does not exist on Android at all**.

The core was already separable (`build_headless.bat` builds it with no `qt-ui`
feature), so the split cost nothing structurally.

## Layout

| Path | What it is |
|---|---|
| `rust/conquerd-android/` | JNI bridge crate — `cdylib`, its own workspace |
| `android/` | Gradle project (Kotlin, Compose, Material 3) |
| `android/app/src/main/jniLibs/` | Where cargo-ndk drops the built `.so` (gitignored) |

The bridge crate depends on `conquerd-client` as a plain library. It never
enables `qt-ui`, which is what keeps Qt out of the Android dependency graph.

## The JNI boundary

Four native methods, on `com.conquerd.client.NativeCore`:

```
String nativeVersion()
long   nativeStart(String homeDir, String passphrase, EventSink sink)
String nativeCommand(long handle, String json)
void   nativeStop(long handle)
```

Everything else rides a **JSON command/event channel**. Kotlin sends
`{"cmd": "...", ...}` and gets `{"ok": true, ...}` or `{"ok": false, "error": "..."}`;
the core pushes events as `{"event": "...", ...}` to `EventSink.onEvent`.

This is deliberate. The desktop bridge exposes ~98 QML invokables; mirroring
each as its own `external fun` would mean changing declarations on both sides of
the boundary every time one moved. One channel means a new feature is a new
`match` arm in `command.rs` and a new `when` branch in Kotlin.

**Real-time media never crosses this boundary.** `SfuAudioReceived`,
`DirectAudioReceived`, `VideoFrameReceived`, `ContentAudioReceived` and
`PortalGameDatagram` are filtered out in `event.rs` — they arrive hundreds of
times a second carrying raw payload bytes, and their pipelines live on the Rust
side. Audio I/O reaches the device through cpal's Oboe backend without touching
JNI at all.

### Threading

Events are pumped by a dedicated OS thread (`conquerd-events`), not a tokio
task. Delivering an event means calling into the JVM, which requires the calling
thread to stay attached — and tokio moves tasks between worker threads freely,
so a task would have to attach and detach around every single event.

## Building

### One-time setup

```powershell
# NDK + a CMake that libopus accepts (CMake 4 rejects its cmake_minimum_required)
sdkmanager "ndk;28.2.13676358" "cmake;3.31.6" "platforms;android-35" "build-tools;35.0.0"

rustup target add aarch64-linux-android
cargo install cargo-ndk
```

Then point `android/local.properties` at the SDK, using **forward slashes** —
Java properties files silently eat single backslashes, which turns
`C:\Users\...` into `C:UsersAWOL...`:

```properties
sdk.dir=C:/Users/you/AppData/Local/Android/Sdk
```

### Build

```powershell
cd android
./gradlew assembleDebug          # or assembleRelease
```

Gradle runs `cargo ndk` itself — `cargoBuildDebug` / `cargoBuildRelease` are
wired ahead of `mergeDebugJniLibFolders`, so one command builds the whole thing.
The Android debug build maps to cargo's dev profile and release to release; a
release APK carrying a dev-profile core would be unusably slow through the Opus
and VP8 paths, which are pure C compiled without SIMD.

To build the core alone:

```powershell
cd rust/conquerd-android
cargo ndk -t arm64-v8a --platform 26 -o ../../android/app/src/main/jniLibs build --lib
```

### ABIs

`gradle.properties` sets `conquerd.abis=arm64-v8a`. Each extra ABI is a full
Rust build of the core *including libopus and libvpx*, so add `x86_64` only when
you actually need the emulator. `conquerd.ndkApi` must stay equal to `minSdk`
(26): cargo-ndk bakes it into the clang target triple, and a mismatch produces a
library `dlopen` refuses on older devices with no useful diagnostic.

## Tests

```powershell
cd rust/conquerd-android && cargo test        # 23, host - no device needed
cd android && ./gradlew testDebugUnitTest     # 13, JVM
```

Both run on any machine: the Rust tests cover the pure halves of the JNI layer
(frame packing and rotation, command parsing, event mapping, media routing) and
the Kotlin tests cover the JSON boundary and the display models.

Two of them exist because of specific regressions and should not be deleted as
redundant:

* `session::tests::inbound_direct_audio_reaches_the_call_controller` - filtering
  media out of the event JSON *without* routing it to `CallController` made
  calls connect, signal correctly and stay completely silent. A wiring bug, so
  only a wiring test catches it.
* `command::tests::every_known_command_has_a_match_arm` - a range edit once
  deleted `room.join` and `room.leave` while leaving them advertised;
  everything still compiled, because a missing arm just falls through to the
  catch-all. The test reads its own source, which is crude, but `dispatch`
  needs a live `Session` and cannot be called from a unit test.

**Not covered, and worth knowing:** the JNI boundary itself (no instrumented
tests), session lifecycle, and anything needing a device or a live core. Those
gaps are why on-device logcat has been the real test harness so far.

## 16 KB page alignment

Android 15+ can run with 16 KB memory pages, and a shared library whose `LOAD`
segments are 4 KB-aligned will not load there. Check every native library in the
APK, not just ours:

```powershell
unzip -o app-debug.apk "lib/arm64-v8a/*"
llvm-readelf -l lib/arm64-v8a/<name>.so | findstr LOAD   # Align must be 0x4000
zipalign -c -P 16 -v 4 app-debug.apk                     # and the zip itself
```

**NDK r28 emits 16 KB-aligned segments by default**, so `libconquerd_android.so`
needs no linker flags. The risk is dependencies: CameraX **1.3.4** shipped
`libimage_processing_util_jni.so` at 4 KB and produced exactly this warning on a
Pixel. Fixed by moving to CameraX **1.4.2**. Any new dependency carrying a `.so`
is worth re-checking with the commands above.

## Install and debug

```powershell
adb install -r android/app/build/outputs/apk/debug/app-debug.apk
adb logcat -s ConquerD
```

Rust logging goes to logcat under the tag `ConquerD` via a `MakeWriter` over
liblog (`logcat.rs`) — an Android process has no stdout anyone can read, so the
default `tracing_subscriber` writer would send every line into the void.

## Cross-compilation fixes this required

Four changes; the first three were latent cross-compilation bugs in shared
crates rather than Android special-casing:

1. **`conquerd-opus/build.rs`** keyed its platform branches off `cfg!(target_os)`,
   which in a build script describes the *host*. It now reads
   `CARGO_CFG_TARGET_OS`, and passes the NDK's `android.toolchain.cmake` plus
   `ANDROID_ABI` / `ANDROID_PLATFORM` — without the toolchain file, cmake's
   Android-Determine module aborts with "Neither the NDK or a standalone
   toolchain was found".
2. **`conquerd-vpx/build.rs`** emitted `cargo:rustc-link-lib=pthread` for every
   non-Windows target. Bionic implements pthreads inside libc and ships no
   `libpthread` at all, so that is a hard link error rather than a no-op.
3. **`arboard`** (clipboard) has no Android backend. It is only ever used by the
   Qt bridge, so it moved to an optional dependency behind the `qt-ui` feature —
   which also drops it from headless desktop builds.
4. **`libc++abi` was never linked** (`rust/conquerd-android/build.rs`).
   `oboe-sys` emits only `-lc++_static`, which supplies libc++ but not the ABI
   layer under it. The link *succeeded* — a shared object may have undefined
   symbols — and failed only on the device as
   `dlopen failed: cannot locate symbol "__cxa_pure_virtual"`. The build script
   now adds `-lc++abi` **and** `-Wl,--no-undefined`, so any future missing
   library is a build error instead of a crash on someone's phone.

## Wireless debugging

Bootstrapping over USB is the shortest path and skips the pairing-code flow:

```powershell
adb tcpip 5555                     # phone must be on USB for this one command
adb connect <phone-ip>:5555        # ip from: adb shell ip -o -f inet addr
adb -s <phone-ip>:5555 install -r android/app/build/outputs/apk/debug/app-debug.apk
```

The phone and the host have to be on the same subnet. `adb tcpip` mode does not
survive a reboot — redo those two commands after one, or use
Developer options > Wireless debugging > *Pair device with pairing code* and
`adb pair` if no cable is available at all.

**With both transports attached, always pass `-s`.** Plain `adb` refuses when
two devices are listed, and scripts that shell out to it will fail on the
ambiguity rather than pick one — `push_android_profile.ps1` takes `-Serial` for
exactly this.

A 35 MB debug APK installs in about four seconds over Wi-Fi, so there is little
reason to stay tethered.

## Using your desktop identity on the phone

`scripts/push_android_profile.ps1` copies an existing profile onto a connected
device — `identity.dat` plus, unless you pass `-IdentityOnly`, `peers.dat`,
`my_rooms.dat` and `chat_history.db`. The stores are encrypted with keys
*derived from* the identity, so they decrypt on the phone once the identity
matches.

```powershell
adb install -r android/app/build/outputs/apk/debug/app-debug.apk
./scripts/push_android_profile.ps1              # or -IdentityOnly
```

App-private storage is not writable by `adb push`, so the script stages through
`/data/local/tmp` and copies in with `run-as` — which only works against a
**debug** build, because release builds are not debuggable.

### This is a move, not a device link

**Do not run two devices on one identity at the same time.** Nothing enforces
it, and the failure is silent. Four places key on identity alone:

| Where | Effect |
|---|---|
| `relay.rs` `peers.insert(identity_pub, ...)` | the second connection evicts the first from the relay map |
| `signaling.rs` `register_quic_sender` | same, last writer wins |
| `main.rs` `on_endpoint_update` | one mailbox entry per identity; devices overwrite each other |
| Room group keys | sealed per *member identity* to a single signaling target, so the losing device never receives the epoch key and — correctly — fails closed, showing nothing in rooms |

There is also no history sync: whichever device is connected receives a
message, and that is the only copy of it.

If you want both devices live at once, give the phone **its own identity** and
trust it as a peer. That is the topology the architecture supports today, and
it lets the two devices message each other. True multi-device (one identity,
many live endpoints) needs per-device subkeys, a device registry, and
per-device group-key sealing — see `backlog.md`.

### The passphrase does not travel

The desktop unlocks from the OS keyring, which caches the *derived AES key*,
not the passphrase. That cache is machine-local, and Android has no Keystore
backend yet, so the phone prompts on every launch and needs the real
passphrase. If it has been auto-unlocking for months, confirm you still know it
before relying on the copy.

## What is wired, and what is not

The per-feature answer lives in [CLIENT_FEATURES.md](CLIENT_FEATURES.md), which
compares every desktop capability against the Android command surface. Keeping
the list in one place stops the two documents disagreeing — as they did while
camera capture, call accept/reject and Keystore auto-unlock landed here but the
list below still called them missing.

What is worth knowing here, because it is Android-specific rather than a
feature gap:

**`ndk_context` must be initialised in `nativeStart` and must stay there.** cpal's
Oboe backend asks it for the JavaVM and Android `Context` when opening a stream;
frameworks like `android-activity` register those, a plain JNI library does not.
Without it the first `StartAudio` panics and kills the `CallController` task, and
every later command then fails on a closed channel far from the real cause.
Relatedly, the panic hook that routes panics to logcat is load-bearing: Android
discards stderr, so a panicking tokio task otherwise dies in complete silence.

**The identity file key, not the passphrase, is what gets stored.** The `keyring`
crate compiles for Android but has no backend there, so `IdentityVault.kt` seals
the key the core exports (`identity.export_key`) with an AndroidKeyStore AES-GCM
key. See the identity section of [CLIENT_FEATURES.md](CLIENT_FEATURES.md).

**A foreground service owns the session.** A peer-to-peer client that dies when
the screen turns off cannot hold a session or receive a message, so the core
runs under a `dataSync` foreground service that upgrades to `microphone` before
capture starts — Android 14+ refuses the type otherwise.

See `backlog.md` for the ordered list of what to build next.
