//! A call's sound and pictures, to and from the phone's audio and camera
//! code (`CallAudio.kt`, `CallVideo.kt`).
//!
//! Sound moves 25 times a second each way and pictures up to 30, far too
//! often for Dart's method channels, and they never need to pass through
//! Dart. So Kotlin calls these functions directly through JNI
//! (`CallNative.kt`). Kotlin loads this same library with
//! `System.loadLibrary`; Android keeps one copy of it per process, shared
//! with the one Dart opened, so these see the same session and network as
//! the rest of the app.
//!
//! None of them waits for the session's lock: the media and the network
//! have their own small locks, held only for a moment.

use crate::api::session::{media, node};
use jni::objects::{JByteArray, JClass};
use jni::sys::{jboolean, jbyteArray, jint, jintArray, JNI_FALSE, JNI_TRUE};
use jni::JNIEnv;
use sanctum_core::call::Pulled;
use sanctum_core::video::FrameInfo;
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

/// [`Java_io_sanctum_sanctum_CallNative_sendVideo`]'s answer: the frame
/// went...
const VIDEO_SENT: jint = 1;
/// ...and the next frame should be a keyframe.
const VIDEO_WANT_KEY: jint = 2;

/// Seals one encoded video frame (H.264 or VP8, as agreed) and sends it to
/// the friend, in pieces. `rotation`: degrees clockwise to show it upright.
/// Returns [`VIDEO_SENT`] and/or [`VIDEO_WANT_KEY`].
#[no_mangle]
pub extern "system" fn Java_io_sanctum_sanctum_CallNative_sendVideo<'local>(
    env: JNIEnv<'local>,
    _class: JClass<'local>,
    frame: JByteArray<'local>,
    key: jboolean,
    rotation: jint,
    width: jint,
    height: jint,
) -> jint {
    let result = catch_unwind(AssertUnwindSafe(|| {
        let Ok(bytes) = env.convert_byte_array(&frame) else {
            return 0;
        };
        let info = FrameInfo {
            key: key != JNI_FALSE,
            rotation: rotation.rem_euclid(360) as u16 / 90 * 90,
            width: width.clamp(0, u16::MAX as jint) as u16,
            height: height.clamp(0, u16::MAX as jint) as u16,
        };
        let Some(node) = node() else { return 0 };
        let sent = node.send_call_video(&bytes, info);
        let mut flags = 0;
        if !sent.packets.is_empty() {
            flags |= VIDEO_SENT;
        }
        if sent.want_key {
            flags |= VIDEO_WANT_KEY;
        }
        flags
    }));
    result.unwrap_or(0)
}

/// What the camera's encoder should do now: `[bits per second, frames per
/// second, picture height, 1 to make the next frame a keyframe]`, or null
/// when our camera shouldn't be sending.
#[no_mangle]
pub extern "system" fn Java_io_sanctum_sanctum_CallNative_videoTarget<'local>(
    env: JNIEnv<'local>,
    _class: JClass<'local>,
) -> jintArray {
    let target = catch_unwind(|| media().and_then(|m| m.video_target()));
    let Ok(Some(t)) = target else {
        return std::ptr::null_mut();
    };
    let values = [t.quality.bitrate as jint, t.quality.fps as jint, t.quality.height as jint, t.key_now as jint];
    let Ok(array) = env.new_int_array(values.len() as i32) else {
        return std::ptr::null_mut();
    };
    if env.set_int_array_region(&array, 0, &values).is_err() {
        return std::ptr::null_mut();
    }
    array.into_raw()
}

/// The next video frame to decode and show, or null if none is due yet: a
/// 6-byte header (1 for a keyframe, quarter turns clockwise, width and
/// height as big-endian 16-bit numbers) and the encoded frame.
#[no_mangle]
pub extern "system" fn Java_io_sanctum_sanctum_CallNative_pullVideo<'local>(
    env: JNIEnv<'local>,
    _class: JClass<'local>,
) -> jbyteArray {
    let frame = catch_unwind(|| media().and_then(|m| m.pull_video()));
    let Ok(Some(f)) = frame else {
        return std::ptr::null_mut();
    };
    let mut bytes = Vec::with_capacity(6 + f.data.len());
    bytes.push(u8::from(f.info.key));
    bytes.push((f.info.rotation / 90 % 4) as u8);
    bytes.extend_from_slice(&f.info.width.to_be_bytes());
    bytes.extend_from_slice(&f.info.height.to_be_bytes());
    bytes.extend_from_slice(&f.data);
    env.byte_array_from_slice(&bytes).map_or(std::ptr::null_mut(), |a| a.into_raw())
}

/// The phone's decoder had to start over: asks the friend for a keyframe.
#[no_mangle]
pub extern "system" fn Java_io_sanctum_sanctum_CallNative_askKeyframe<'local>(
    _env: JNIEnv<'local>,
    _class: JClass<'local>,
) {
    let _ = catch_unwind(|| {
        if let Some(m) = media() {
            m.ask_keyframe();
        }
        if let Some(n) = node() {
            n.flush_media();
        }
    });
}
