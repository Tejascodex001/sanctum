package io.sanctum.sanctum

/**
 * A call's sound and pictures to and from Rust (app/rust/src/call_jni.rs),
 * straight through JNI: many packets a second each way, never through
 * Dart. Rust seals each packet with the call's key before it leaves the
 * phone.
 *
 * This loads the same library Dart already opened; Android keeps one copy
 * per process, so Rust sees the same session and network here.
 */
object CallNative {
    init {
        System.loadLibrary("rust_lib_sanctum")
    }

    /** Seals and sends 40 ms of encoded sound. False if it couldn't go. */
    @JvmStatic
    external fun send(packet: ByteArray): Boolean

    /**
     * The next 40 ms to play: a packet, an empty array when it was lost
     * (fill the gap), or null when there's nothing to play yet (silence).
     */
    @JvmStatic
    external fun pull(): ByteArray?

    /**
     * Seals and sends one encoded video frame. `rotation`: degrees clockwise
     * to show it upright. Returns [VIDEO_SENT] and/or [VIDEO_WANT_KEY].
     */
    @JvmStatic
    external fun sendVideo(frame: ByteArray, key: Boolean, rotation: Int, width: Int, height: Int): Int

    /**
     * What the encoder should do: [bits per second, frames per second,
     * picture height, 1 for a keyframe now], or null while our camera
     * shouldn't send.
     */
    @JvmStatic
    external fun videoTarget(): IntArray?

    /**
     * The next frame to decode, once its sound is playing: a 6-byte header
     * (1 for a keyframe, quarter turns clockwise, width and height as
     * big-endian 16-bit numbers) then the frame; null if none is due.
     */
    @JvmStatic
    external fun pullVideo(): ByteArray?

    /** The decoder had to start over: asks the friend for a keyframe. */
    @JvmStatic
    external fun askKeyframe()

    /** Whether the library loaded (it always should). */
    fun ready(): Boolean = runCatching { pull(); true }.getOrDefault(false)

    const val VIDEO_SENT = 1
    const val VIDEO_WANT_KEY = 2
}
