package io.sanctum.sanctum

/**
 * A call's sound to and from Rust (app/rust/src/call_jni.rs), straight
 * through JNI: 25 packets a second each way, never through Dart. Rust seals
 * each packet with the call's key before it leaves the phone.
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

    /** Whether the library loaded (it always should). */
    fun ready(): Boolean = runCatching { pull(); true }.getOrDefault(false)
}
