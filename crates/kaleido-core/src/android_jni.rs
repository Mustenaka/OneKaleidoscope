//! Android-only JNI boundary for installing iroh's system DNS context.

#![allow(
    unsafe_code,
    reason = "this module is the audited Android JNI FFI boundary"
)]

use std::ffi::c_void;
use std::sync::Mutex;

use jni::errors::ThrowRuntimeExAndDefault;
use jni::objects::{Global, JObject};
use jni::sys::{jboolean, JNI_FALSE, JNI_TRUE};
use jni::EnvUnowned;

struct InstalledAndroidContext {
    _application_context: Global<JObject<'static>>,
}

static INSTALLED_CONTEXT: Mutex<Option<InstalledAndroidContext>> = Mutex::new(None);

/// Installs the process-lifetime Android application context used by iroh DNS.
///
/// Kotlin calls this exactly once from `Application.onCreate`, before any
/// `MobileClient` can construct a remote endpoint. Repeated calls are
/// idempotent. JNI errors and panics are converted into a Java runtime
/// exception and `false`, so callers can fail closed.
#[unsafe(no_mangle)]
pub extern "system" fn Java_com_onekaleidoscope_AndroidIrohJni_install<'local>(
    mut unowned_env: EnvUnowned<'local>,
    _instance: JObject<'local>,
    application_context: JObject<'local>,
) -> jboolean {
    unowned_env
        .with_env(|env| -> jni::errors::Result<jboolean> {
            let mut installed = match INSTALLED_CONTEXT.lock() {
                Ok(installed) => installed,
                Err(_) => return Ok(JNI_FALSE),
            };
            if installed.is_some() {
                return Ok(JNI_TRUE);
            }
            if application_context.is_null() {
                return Ok(JNI_FALSE);
            }

            let global_context = env.new_global_ref(&application_context)?;
            let java_vm = env.get_java_vm()?;
            let java_vm_pointer = java_vm.get_raw().cast::<c_void>();
            let context_pointer = global_context.as_obj().as_raw().cast::<c_void>();

            // SAFETY: `java_vm_pointer` is supplied by the currently attached
            // JVM. `global_context` is a strong JNI global reference and is
            // moved into a process-static holder immediately below, so both
            // pointers remain valid for the entire Android process lifetime.
            unsafe {
                iroh::dns::install_android_jni_context(java_vm_pointer, context_pointer);
            }
            crate::connection::remote_tunnel::mark_android_jni_context_installed();
            *installed = Some(InstalledAndroidContext {
                _application_context: global_context,
            });
            Ok(JNI_TRUE)
        })
        .resolve::<ThrowRuntimeExAndDefault>()
}
