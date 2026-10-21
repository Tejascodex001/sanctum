//! A call's sound, to and from the phone's audio code (`CallAudio.kt`).
//!
//! Sound moves 25 times a second each way, far too often for Dart's method
//! channels, and it never needs to pass through Dart. So Kotlin calls these
//! two functions directly through JNI (`CallNative.kt`). Kotlin loads this
//! same library with `System.loadLibrary`; Android keeps one copy of it per
//! process, shared with the one Dart opened, so these see the same session
//! and network as the rest of the app.
//!
//! Neither function waits for the session's lock: the media and the network
//! have their own small locks, held only for a moment.

use crate::api::session::{media, node};
use jni::objects::{JByteArray, JClass};
use jni::sys::{jboolean, jbyteArray, JNI_FALSE, JNI_TRUE};
use jni::JNIEnv;
use sanctum_core::call::Pulled;
use std::panic::{catch_unwind, AssertUnwindSafe};

/// Seals 40 ms of encoded sound and sends it to the friend in the call.
/// False if it couldn't go (no call, no link, or the link is behind).
#[no_mangle]
pub extern "system" fn Java_io_sanctum_sanctum_CallNative_send<'local>(
    env: JNIEnv<'local>,
    _class: JClass<'local>,
    packet: JByteArray<'local>,
) -> jboolean {
    let sent = catch_unwind(AssertUnwindSafe(|| {
        let Ok(bytes) = env.convert_byte_array(&packet) else {
            return false;
        };
        node().is_some_and(|n| n.send_call_audio(&bytes))
    }));
    if matches!(sent, Ok(true)) {
        JNI_TRUE
    } else {
        JNI_FALSE
    }
}

/// The next 40 ms to play: the packet, an empty array when it was lost (the
/// player fills the gap), or null when there's nothing to play yet.
#[no_mangle]
pub extern "system" fn Java_io_sanctum_sanctum_CallNative_pull<'local>(
    env: JNIEnv<'local>,
    _class: JClass<'local>,
) -> jbyteArray {
    let pulled = catch_unwind(|| media().map_or(Pulled::Silence, |m| m.pull_audio()));
    let bytes = match pulled {
        Ok(Pulled::Packet(data)) => data,
        Ok(Pulled::Lost) => Vec::new(),
        _ => return std::ptr::null_mut(),
    };
    env.byte_array_from_slice(&bytes).map_or(std::ptr::null_mut(), |a| a.into_raw())
}
