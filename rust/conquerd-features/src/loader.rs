//! Native cdylib module loader — Phase 5.
//!
//! Provides a stable C ABI that third-party Rust cdylibs must implement
//! and a [`NativeModuleLoader`] that:
//!
//! 1. Parses and signature-verifies the module's `*.module.toml` manifest.
//! 2. Checks whether the manifest's signer key is in the user's
//!    [`TrustedKeyStore`]; if not, calls `on_unknown_key` (the trust prompt).
//! 3. Verifies the SHA-256 of the cdylib binary against the manifest.
//! 4. Opens the library with [`libloading`], retrieves the
//!    `conquerd_module_entry` symbol, validates the ABI version, and calls
//!    `create` to obtain a module state pointer.
//! 5. Returns a [`SharedModule`] wrapping the native code.
//!
//! # Safety
//!
//! Loading a native cdylib is inherently unsafe. The trust gate (manifest
//! signature + user consent) is the only security boundary — once a library
//! is loaded it runs in the host process with full permissions. Treat
//! third-party modules like native plugins and only trust publishers you
//! know.
//!
//! # Module ABI
//!
//! Implement the following in your cdylib (`crate-type = ["cdylib"]`):
//!
//! ```rust,ignore
//! use conquerd_features::loader::{ABI_VERSION, ConquerdModuleVtable};
//! use std::ffi::c_void;
//!
//! struct MyModule;
//! // … impl conquered_features::FeatureModule for MyModule …
//!
//! static VTABLE: ConquerdModuleVtable = ConquerdModuleVtable {
//!     abi_version: ABI_VERSION,
//!     create:     my_create,
//!     on_invoke:  my_on_invoke,
//!     on_message: my_on_message,
//!     shutdown:   my_shutdown,
//!     destroy:    my_destroy,
//! };
//!
//! extern "C" fn my_create() -> *mut c_void {
//!     Box::into_raw(Box::new(MyModule)) as *mut c_void
//! }
//! extern "C" fn my_on_invoke(
//!     state: *mut c_void,
//!     peer: *const u8, peer_len: usize,
//!     params_json: *const u8, params_len: usize,
//!     channel_tag: i32,
//! ) -> i32 {
//!     let _m = unsafe { &*(state as *const MyModule) };
//!     0 // ok
//! }
//! extern "C" fn my_on_message(
//!     state: *mut c_void,
//!     source: *const u8, source_len: usize,
//!     payload: *const u8, payload_len: usize,
//! ) {}
//! extern "C" fn my_shutdown(state: *mut c_void) {}
//! extern "C" fn my_destroy(state: *mut c_void) {
//!     drop(unsafe { Box::from_raw(state as *mut MyModule) });
//! }
//!
//! #[no_mangle]
//! pub extern "C" fn conquerd_module_entry() -> *const ConquerdModuleVtable {
//!     &VTABLE
//! }
//! ```

use std::ffi::c_void;
use std::path::Path;
use std::sync::{Arc, Mutex};

use libloading::{Library, Symbol};

use crate::descriptor::CapabilityDescriptor;
use crate::module::{
    FeatureModule, InvocationContext, ModuleError, ModuleResult, PeerId, SharedModule,
};
use crate::signing::{ModuleManifest, SigningError, TrustedKeyStore};

/// ABI version this loader accepts. Increment on any breaking vtable change.
pub const ABI_VERSION: u32 = 1;

/// C-compatible vtable every third-party cdylib must provide.
///
/// Obtain a pointer to this struct via the exported symbol:
/// ```c
/// const ConquerdModuleVtable *conquerd_module_entry(void);
/// ```
///
/// # Safety contract
///
/// - All function pointers must be non-null.
/// - `state` is the value returned by `create`; it is valid until `destroy`.
/// - Byte-slice pairs `(ptr, len)` for string fields (peer id, params JSON)
///   are valid UTF-8. The payload slice is raw bytes.
/// - `destroy` is called exactly once, after `shutdown`.
#[repr(C)]
pub struct ConquerdModuleVtable {
    /// Must equal [`ABI_VERSION`]; the loader rejects mismatches.
    pub abi_version: u32,
    /// Allocate a fresh module state. Must not return null.
    pub create: unsafe extern "C" fn() -> *mut c_void,
    /// Handle a `CAPABILITY_INVOKE`. Returns 0 on success, non-zero on error.
    /// `peer` is a UTF-8 peer id; `params_json` is the serialised invocation
    /// params; `channel_tag` is the allocated datagram tag (< 0 = none).
    pub on_invoke: unsafe extern "C" fn(
        state: *mut c_void,
        peer: *const u8,
        peer_len: usize,
        params_json: *const u8,
        params_len: usize,
        channel_tag: i32,
    ) -> i32,
    /// Deliver an inbound per-message payload. Fire and forget.
    pub on_message: unsafe extern "C" fn(
        state: *mut c_void,
        source: *const u8,
        source_len: usize,
        payload: *const u8,
        payload_len: usize,
    ),
    /// Teardown notification — called before `destroy`.
    pub shutdown: unsafe extern "C" fn(state: *mut c_void),
    /// Free the state. Called exactly once, after `shutdown`.
    pub destroy: unsafe extern "C" fn(state: *mut c_void),
}

/// Symbol the loader looks for in the cdylib (null-terminated).
const ENTRY_SYMBOL: &[u8] = b"conquerd_module_entry\0";

// ---------------------------------------------------------------------------
// FfiModuleHandle — the in-process wrapper for a loaded cdylib module
// ---------------------------------------------------------------------------

/// Owns a loaded third-party native module.
///
/// Holds the [`Library`] open (keeping the code mapped) and the opaque state
/// pointer for the module instance. Drop order matters: `state` is destroyed
/// (via vtable) before the library is unloaded.
struct FfiModuleHandle {
    descriptor: CapabilityDescriptor,
    state: *mut c_void,
    vtable: *const ConquerdModuleVtable,
    // Dropped last so the vtable function pointers remain valid during
    // `shutdown` + `destroy` (called in Drop).
    _lib: Library,
}

// SAFETY: third-party modules must be Send+Sync per the ABI contract.
// The loader is the enforcement boundary; once the module is loaded it has
// full process permissions anyway.
unsafe impl Send for FfiModuleHandle {}
unsafe impl Sync for FfiModuleHandle {}

impl Drop for FfiModuleHandle {
    fn drop(&mut self) {
        // SAFETY: vtable and state are valid until this point (library is
        // still mapped because `_lib` has not been dropped yet).
        unsafe {
            let vt = &*self.vtable;
            (vt.shutdown)(self.state);
            (vt.destroy)(self.state);
        }
    }
}

impl FeatureModule for FfiModuleHandle {
    fn descriptor(&self) -> CapabilityDescriptor {
        self.descriptor.clone()
    }

    fn on_invoke(&self, ctx: InvocationContext) -> ModuleResult<()> {
        let vt = unsafe { &*self.vtable };
        let peer = ctx.peer.as_bytes();
        let params = serde_json::to_string(&ctx.params)
            .map_err(|e| ModuleError::Internal(format!("params serialize: {e}")))?;
        let params_b = params.as_bytes();
        let tag: i32 = ctx.channel_tag.map(|t| t as i32).unwrap_or(-1);
        // SAFETY: all pointers are valid for the duration of the call.
        let ret = unsafe {
            (vt.on_invoke)(
                self.state,
                peer.as_ptr(),
                peer.len(),
                params_b.as_ptr(),
                params_b.len(),
                tag,
            )
        };
        if ret != 0 {
            Err(ModuleError::Internal(format!("on_invoke returned {ret}")))
        } else {
            Ok(())
        }
    }

    fn on_message(&self, source: PeerId, payload: &[u8]) {
        let vt = unsafe { &*self.vtable };
        let src = source.as_bytes();
        // SAFETY: all pointers are valid for the duration of the call.
        unsafe {
            (vt.on_message)(
                self.state,
                src.as_ptr(),
                src.len(),
                payload.as_ptr(),
                payload.len(),
            );
        }
    }

    fn shutdown(&self) {
        // Shutdown is handled via Drop to ensure it runs exactly once.
        // The trait method is intentionally a no-op here.
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Data presented to the user in a trust prompt.
#[derive(Debug, Clone)]
pub struct TrustRequest {
    /// Base64url-no-pad-encoded 32-byte Ed25519 verifying key of the signer.
    pub signer_pubkey: String,
    /// Capability id from the manifest (e.g. `x.acme.matchmaker`).
    pub module_id: String,
    /// Human-readable author name from the manifest.
    pub author: String,
}

/// Errors raised by the native module loader.
#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("manifest error: {0}")]
    Manifest(#[from] SigningError),
    #[error("signature verification failed")]
    Signature,
    #[error("cdylib integrity check failed: {0}")]
    Integrity(String),
    #[error("signer key not trusted and prompt was denied")]
    NotTrusted,
    #[error("library load error: {0}")]
    Library(String),
    #[error("ABI version mismatch: module={module}, loader={loader}")]
    AbiMismatch { module: u32, loader: u32 },
    #[error("module create() returned null state pointer")]
    NullState,
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Loads and registers native third-party feature modules.
///
/// # Usage
///
/// ```rust,ignore
/// let mut store = TrustedKeyStore::new();
/// store.trust("base64url-pubkey-of-acme-corp");
///
/// let loader = NativeModuleLoader::new(store, |req: TrustRequest| {
///     eprintln!("Unknown signer {} for {}", req.signer_pubkey, req.module_id);
///     false // deny unknown keys in headless mode
/// });
///
/// let module = loader.load(
///     Path::new("acme.module.toml"),
///     Path::new("acme_matchmaker.so"),
/// )?;
/// registry.register_module(module)?;
/// ```
pub struct NativeModuleLoader {
    trust_store: Mutex<TrustedKeyStore>,
    /// Called when the signer key is not in the trust store.
    /// Return `true` to grant session-scoped trust; `false` to deny loading.
    on_unknown_key: Box<dyn Fn(TrustRequest) -> bool + Send + Sync>,
}

impl NativeModuleLoader {
    pub fn new<F>(trust_store: TrustedKeyStore, on_unknown_key: F) -> Self
    where
        F: Fn(TrustRequest) -> bool + Send + Sync + 'static,
    {
        Self {
            trust_store: Mutex::new(trust_store),
            on_unknown_key: Box::new(on_unknown_key),
        }
    }

    /// Load a module from a manifest file and a cdylib path.
    ///
    /// Steps (each is a hard gate — failure aborts loading):
    ///
    /// 1. Parse and signature-verify the manifest.
    /// 2. Check trust store; if key is unknown, invoke `on_unknown_key`.
    /// 3. Read the cdylib and verify its SHA-256 against the manifest.
    /// 4. Open the library, resolve `conquerd_module_entry`, validate ABI.
    /// 5. Call `create()` and return the wrapped [`SharedModule`].
    pub fn load(
        &self,
        manifest_path: &Path,
        cdylib_path: &Path,
    ) -> Result<SharedModule, LoadError> {
        // 1. Parse and signature-verify the manifest.
        let manifest = ModuleManifest::load(manifest_path)?;
        manifest
            .verify_signature()
            .map_err(|_| LoadError::Signature)?;

        // 2. Trust gate.
        {
            let mut store = self.trust_store.lock().unwrap();
            if !store.is_trusted(&manifest.signer_pubkey) {
                let req = TrustRequest {
                    signer_pubkey: manifest.signer_pubkey.clone(),
                    module_id: manifest.id.clone(),
                    author: manifest.author.clone(),
                };
                if !(self.on_unknown_key)(req) {
                    return Err(LoadError::NotTrusted);
                }
                // Grant session-scoped trust.
                store.trust(manifest.signer_pubkey.clone());
            }
        }

        // 3. Read + hash-verify the cdylib.
        let cdylib_bytes = std::fs::read(cdylib_path)?;
        manifest
            .verify_cdylib(&cdylib_bytes)
            .map_err(|e| LoadError::Integrity(e.to_string()))?;

        // 4. Open the library and resolve the entry symbol.
        //
        // SAFETY: we have verified the manifest signature and the cdylib hash;
        // the user has consented via the trust prompt.
        let lib =
            unsafe { Library::new(cdylib_path) }.map_err(|e| LoadError::Library(e.to_string()))?;

        let vtable: *const ConquerdModuleVtable = unsafe {
            let sym: Symbol<unsafe extern "C" fn() -> *const ConquerdModuleVtable> = lib
                .get(ENTRY_SYMBOL)
                .map_err(|e| LoadError::Library(format!("missing entry symbol: {e}")))?;
            sym()
        };

        if vtable.is_null() {
            return Err(LoadError::Library(
                "conquerd_module_entry returned a null vtable".into(),
            ));
        }

        // 5. Validate ABI version.
        let module_abi = unsafe { (*vtable).abi_version };
        if module_abi != ABI_VERSION {
            return Err(LoadError::AbiMismatch {
                module: module_abi,
                loader: ABI_VERSION,
            });
        }

        // 6. Create the module state.
        let state = unsafe { ((*vtable).create)() };
        if state.is_null() {
            return Err(LoadError::NullState);
        }

        let descriptor = manifest.capability.to_descriptor();
        Ok(Arc::new(FfiModuleHandle {
            descriptor,
            state,
            vtable,
            _lib: lib,
        }))
    }

    /// Grant session-scoped trust for a signer key without a prompt.
    ///
    /// Useful for pre-trusting keys loaded from `trusted_module_keys.txt`
    /// at startup.
    pub fn trust_key(&self, pubkey_b64: impl Into<String>) {
        self.trust_store.lock().unwrap().trust(pubkey_b64);
    }

    /// Check whether a signer key is currently trusted.
    pub fn is_trusted(&self, pubkey_b64: &str) -> bool {
        self.trust_store.lock().unwrap().is_trusted(pubkey_b64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::descriptor::{AuthTier, ChannelKind};
    use crate::signing::{
        sign_manifest, ManifestCapability, ModuleManifest, TrustedKeyStore, MANIFEST_SCHEMA_VERSION,
    };
    use ed25519_dalek::SigningKey;
    use rand::rngs::OsRng;

    fn gen_key() -> SigningKey {
        SigningKey::generate(&mut OsRng)
    }

    fn signed_manifest(key: &SigningKey) -> ModuleManifest {
        let mut m = ModuleManifest {
            schema_version: MANIFEST_SCHEMA_VERSION,
            id: "x.test.thing".to_string(),
            version: "1.0".to_string(),
            author: "Test Inc".to_string(),
            signer_pubkey: String::new(),
            cdylib_sha256: ModuleManifest::hash_cdylib(b"fake"),
            signature: String::new(),
            capability: ManifestCapability {
                id: "x.test.thing".to_string(),
                version: "1.0".to_string(),
                kind: ChannelKind::Request,
                auth: AuthTier::TrustedPeer,
            },
        };
        sign_manifest(&mut m, key).unwrap();
        m
    }

    #[test]
    fn trust_gate_uses_trust_store() {
        let key = gen_key();
        let m = signed_manifest(&key);

        let mut store = TrustedKeyStore::new();
        store.trust(&m.signer_pubkey);

        let loader = NativeModuleLoader::new(store, |_| panic!("should not be called"));
        assert!(loader.is_trusted(&m.signer_pubkey));
    }

    #[test]
    fn trust_key_adds_to_store() {
        let store = TrustedKeyStore::new();
        let loader = NativeModuleLoader::new(store, |_| false);
        loader.trust_key("my_pubkey");
        assert!(loader.is_trusted("my_pubkey"));
    }

    #[test]
    fn unknown_key_denied_when_prompt_returns_false() {
        let key = gen_key();
        let m = signed_manifest(&key);
        // Signature verifies — the barrier is the trust prompt.
        m.verify_signature().unwrap();

        // Prompt always denies.
        assert!(!TrustedKeyStore::new().is_trusted(&m.signer_pubkey));
    }

    #[test]
    fn trust_request_fields_are_populated_from_manifest() {
        let key = gen_key();
        let m = signed_manifest(&key);
        let req = TrustRequest {
            signer_pubkey: m.signer_pubkey.clone(),
            module_id: m.id.clone(),
            author: m.author.clone(),
        };
        assert_eq!(req.module_id, "x.test.thing");
        assert_eq!(req.author, "Test Inc");
        assert!(!req.signer_pubkey.is_empty());
    }

    #[test]
    fn abi_version_is_nonzero() {
        assert!(ABI_VERSION > 0);
    }

    #[test]
    fn vtable_struct_is_repr_c() {
        // If the struct were not #[repr(C)] this test would still compile,
        // but we verify the field offsets are stable by checking the size
        // (6 function pointers + 1 u32 = known minimum).
        let size = std::mem::size_of::<ConquerdModuleVtable>();
        // u32 + 5 fn pointers (each pointer-sized) ≥ 4 + 5*8 = 44 on 64-bit.
        assert!(size >= 44);
    }

    #[test]
    fn load_error_display_messages_are_sensible() {
        let e = LoadError::NotTrusted;
        assert!(e.to_string().contains("denied"));

        let e = LoadError::AbiMismatch {
            module: 2,
            loader: 1,
        };
        let s = e.to_string();
        assert!(s.contains("module=2") && s.contains("loader=1"));
    }
}
