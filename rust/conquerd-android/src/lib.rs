//! JNI entry points for the ConquerD Android client.
//!
//! The Kotlin side sees four `native` methods on `com.conquerd.client.NativeCore`:
//!
//! * `nativeVersion()` — the core version string; also a cheap check that the
//!   library loaded at all.
//! * `nativeStart(homeDir, passphrase, sink)` — unlock the identity, open the
//!   stores, start the core. Returns an opaque handle.
//! * `nativeCommand(handle, json)` — run one command, return one JSON reply.
//! * `nativeStop(handle)` — shut the core down.
//!
//! Everything else flows through the JSON command/event channel documented in
//! [`command`] and [`event`], so adding a feature does not mean adding a JNI
//! signature on both sides of the boundary.

mod command;
mod event;
mod session;
mod sink;

#[cfg(target_os = "android")]
mod logcat;

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Once;

use jni::objects::{JClass, JObject, JString};
use jni::sys::{jlong, jstring};
use jni::JNIEnv;
use tracing::{error, info};

use crate::session::Session;
use crate::sink::EventSink;

/// Java exception thrown when start-up fails.
const RUNTIME_EXCEPTION: &str = "java/lang/RuntimeException";

static LOGGING: Once = Once::new();

/// Install the tracing subscriber the first time we are called into.
///
/// `RUST_LOG` still works for on-device debugging via
/// `adb shell setprop log.tag.ConquerD VERBOSE`-style workflows; the default
/// keeps the core at info and silences dependency noise.
fn init_logging() {
    LOGGING.call_once(|| {
        let filter = tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info,conquerd_client=debug"));

        let builder = tracing_subscriber::fmt()
            .with_env_filter(filter)
            // logcat stamps and colours its own output.
            .with_ansi(false)
            .without_time();

        #[cfg(target_os = "android")]
        let result = builder.with_writer(crate::logcat::LogcatWriter).try_init();

        #[cfg(not(target_os = "android"))]
        let result = builder.try_init();

        if result.is_err() {
            // Another subscriber is already installed. Harmless — it just
            // means someone set logging up before us.
        }

        install_panic_hook();
    });
}

/// Route panics into logcat.
///
/// Rust prints panics to stderr, and an Android process has no stderr anyone
/// can read — so a panic inside a spawned task is completely invisible: the
/// task dies, its channel receiver drops, and the only symptom is that later
/// `try_send`s start failing somewhere unrelated. That is exactly how a
/// panicking audio pipeline presents as "could not start audio".
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // `PanicHookInfo::payload_as_str` is not stable here, so recover the
        // message the long way round.
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_owned())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "<non-string panic payload>".to_owned());

        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "<unknown location>".to_owned());

        error!("PANIC at {location}: {message}");
        previous(info);
    }));
}

/// Turn a handle back into a session reference.
///
/// # Safety
///
/// `handle` must be a value returned by `nativeStart` that has not yet been
/// passed to `nativeStop`. Kotlin holds it in a single field guarded by the
/// core's own lifecycle, so a stale handle is a bug on that side rather than
/// something this can validate.
unsafe fn session_from(handle: jlong) -> Option<&'static Session> {
    if handle == 0 {
        return None;
    }
    (handle as *const Session).as_ref()
}

/// Read a Java string, or `None` if it was null or not decodable.
fn read_string(env: &mut JNIEnv<'_>, value: &JString<'_>) -> Option<String> {
    env.get_string(value).ok().map(Into::into)
}

/// `String NativeCore.nativeVersion()`
#[no_mangle]
pub extern "system" fn Java_com_conquerd_client_NativeCore_nativeVersion<'local>(
    env: JNIEnv<'local>,
    _class: JClass<'local>,
) -> jstring {
    init_logging();
    match env.new_string(env!("CARGO_PKG_VERSION")) {
        Ok(s) => s.into_raw(),
        Err(_) => JObject::null().into_raw(),
    }
}

/// Hand the JavaVM and Android `Context` to `ndk-context`.
///
/// cpal's Android backend is Oboe, which reaches for both through
/// `ndk_context::android_context()` when opening a stream. Frameworks like
/// `android-activity` register them at start-up; a plain JNI library has no
/// such entry point, so without this the first `StartAudio` panics with
/// "android context was not initialized", killing the `CallController` task —
/// after which every later command fails on a closed channel, far from the
/// real cause.
///
/// # Safety
///
/// `ndk-context` stores both pointers process-globally and hands them to Oboe
/// on any thread, so the `Context` reference must stay valid for the life of
/// the process. The global reference is therefore deliberately leaked rather
/// than dropped at the end of this call.
#[cfg(target_os = "android")]
fn initialize_ndk_context(env: &JNIEnv<'_>, context: &JObject<'_>) -> jni::errors::Result<()> {
    static INITIALIZED: Once = Once::new();

    let vm = env.get_java_vm()?;
    let global = env.new_global_ref(context)?;

    INITIALIZED.call_once(|| {
        // SAFETY: both pointers outlive the process — the JavaVM is owned by
        // the runtime, and `global` is leaked immediately below.
        unsafe {
            ndk_context::initialize_android_context(
                vm.get_java_vm_pointer().cast(),
                global.as_obj().as_raw().cast(),
            );
        }
        std::mem::forget(global);
    });

    Ok(())
}

/// `long NativeCore.nativeStart(String homeDir, String passphrase, Context context, EventSink sink)`
///
/// Returns 0 and throws on failure.
#[no_mangle]
pub extern "system" fn Java_com_conquerd_client_NativeCore_nativeStart<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    home_dir: JString<'local>,
    passphrase: JString<'local>,
    context: JObject<'local>,
    listener: JObject<'local>,
) -> jlong {
    init_logging();

    // Before anything can start audio.
    #[cfg(target_os = "android")]
    if let Err(e) = initialize_ndk_context(&env, &context) {
        let _ = env.throw_new(
            RUNTIME_EXCEPTION,
            format!("could not hand the Android context to the audio backend: {e}"),
        );
        return 0;
    }
    #[cfg(not(target_os = "android"))]
    let _ = &context;

    let Some(home_dir) = read_string(&mut env, &home_dir) else {
        let _ = env.throw_new(RUNTIME_EXCEPTION, "homeDir must not be null");
        return 0;
    };
    // A null passphrase is legitimate: it means an unencrypted identity, the
    // same as the desktop client's empty-passphrase path.
    let passphrase = read_string(&mut env, &passphrase).unwrap_or_default();

    let sink = match EventSink::new(&env, &listener) {
        Ok(s) => s,
        Err(e) => {
            let _ = env.throw_new(RUNTIME_EXCEPTION, format!("could not hold event sink: {e}"));
            return 0;
        }
    };

    // A panic unwinding into the JVM is undefined behaviour, so start-up is
    // fenced: any panic below becomes a Java exception instead.
    let started = catch_unwind(AssertUnwindSafe(|| {
        Session::start(&home_dir, &passphrase, sink)
    }));

    match started {
        Ok(Ok(session)) => {
            info!("core started");
            Box::into_raw(Box::new(session)) as jlong
        }
        Ok(Err(e)) => {
            error!("core failed to start: {e}");
            let _ = env.throw_new(RUNTIME_EXCEPTION, format!("{e}"));
            0
        }
        Err(_) => {
            error!("core panicked during start-up");
            let _ = env.throw_new(
                RUNTIME_EXCEPTION,
                "the client core panicked during start-up",
            );
            0
        }
    }
}

/// `String NativeCore.nativeCommand(long handle, String json)`
#[no_mangle]
pub extern "system" fn Java_com_conquerd_client_NativeCore_nativeCommand<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    handle: jlong,
    request: JString<'local>,
) -> jstring {
    // SAFETY: see `session_from`.
    let Some(session) = (unsafe { session_from(handle) }) else {
        return reply(&mut env, r#"{"ok":false,"error":"no running session"}"#);
    };

    let Some(request) = read_string(&mut env, &request) else {
        return reply(
            &mut env,
            r#"{"ok":false,"error":"command must not be null"}"#,
        );
    };

    let result = catch_unwind(AssertUnwindSafe(|| command::dispatch(session, &request)));

    match result {
        Ok(value) => {
            let json = serde_json::to_string(&value)
                .unwrap_or_else(|_| r#"{"ok":false,"error":"reply was not encodable"}"#.to_owned());
            reply(&mut env, &json)
        }
        Err(_) => {
            error!("command handler panicked");
            reply(
                &mut env,
                r#"{"ok":false,"error":"the command handler panicked"}"#,
            )
        }
    }
}

/// `void NativeCore.nativeStop(long handle)`
#[no_mangle]
pub extern "system" fn Java_com_conquerd_client_NativeCore_nativeStop<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
    handle: jlong,
) {
    if handle == 0 {
        return;
    }

    // SAFETY: reclaims the box `nativeStart` leaked. Kotlin clears its handle
    // field before calling, so this runs once per session.
    let session = unsafe { Box::from_raw(handle as *mut Session) };

    if catch_unwind(AssertUnwindSafe(|| session.stop())).is_err() {
        error!("core panicked during shutdown");
    }
}

/// Encode a reply string, falling back to null if the JVM refuses the
/// allocation (which in practice means it is already out of memory).
fn reply(env: &mut JNIEnv<'_>, json: &str) -> jstring {
    match env.new_string(json) {
        Ok(s) => s.into_raw(),
        Err(e) => {
            error!("could not allocate reply string: {e}");
            JObject::null().into_raw()
        }
    }
}
