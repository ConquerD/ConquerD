//! Delivering core events back into Kotlin.
//!
//! The Kotlin side hands `nativeStart` an object implementing
//! `com.conquerd.client.NativeCore$EventSink`; every core event is forwarded to
//! its `onEvent(String)` as JSON.

use jni::objects::{GlobalRef, JObject, JValue};
use jni::{JNIEnv, JavaVM};
use tracing::warn;

/// JNI signature of `void onEvent(String)`.
const ON_EVENT_SIG: &str = "(Ljava/lang/String;)V";

/// A thread-safe handle to the Kotlin event listener.
///
/// Holds the `JavaVM` rather than a `JNIEnv` because a `JNIEnv` is only valid
/// on the thread that produced it, and events are emitted from the pump thread
/// rather than from whichever thread called into JNI.
pub struct EventSink {
    vm: JavaVM,
    listener: GlobalRef,
}

impl EventSink {
    /// Promote a local listener reference to a global one that survives the
    /// return from `nativeStart`.
    pub fn new(env: &JNIEnv<'_>, listener: &JObject<'_>) -> jni::errors::Result<Self> {
        Ok(Self {
            vm: env.get_java_vm()?,
            listener: env.new_global_ref(listener)?,
        })
    }

    /// Attach the calling thread to the JVM for as long as the returned guard
    /// lives.
    ///
    /// The pump thread calls this once at start-up rather than per event:
    /// attaching and detaching around every message costs a JVM thread
    /// registration each time, and chat-heavy traffic makes that measurable.
    pub fn attach(&self) -> jni::errors::Result<jni::AttachGuard<'_>> {
        self.vm.attach_current_thread()
    }

    /// Deliver one JSON event to Kotlin.
    ///
    /// Errors are logged and swallowed: a listener that throws must not take
    /// down the connection manager feeding it.
    pub fn emit(&self, env: &mut JNIEnv<'_>, json: &str) {
        let payload = match env.new_string(json) {
            Ok(s) => s,
            Err(e) => {
                warn!("could not allocate event string: {e}");
                return;
            }
        };

        let result = env.call_method(
            &self.listener,
            "onEvent",
            ON_EVENT_SIG,
            &[JValue::Object(&payload)],
        );

        if let Err(e) = result {
            warn!("event listener threw: {e}");
            // A pending exception poisons every later JNI call on this thread,
            // so it has to be cleared even though we are discarding it.
            let _ = env.exception_clear();
        }
    }
}
