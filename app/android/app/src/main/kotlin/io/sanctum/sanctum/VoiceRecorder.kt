package io.sanctum.sanctum

import android.content.Context
import android.media.MediaRecorder
import android.os.Build
import android.os.SystemClock
import java.io.File

/**
 * Records voice notes: AAC in an .m4a, mono, small enough for Tor. The
 * Rust side encrypts the file and Sanctum deletes this copy.
 */
class VoiceRecorder(private val context: Context) {
    private var recorder: MediaRecorder? = null
    private var file: File? = null
    private var startedAt = 0L

    fun start(): Boolean {
        cancel()
        val out = File(context.cacheDir, "outgoing/voice-${System.nanoTime()}.m4a")
        out.parentFile?.mkdirs()
        val r = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            MediaRecorder(context)
        } else {
            @Suppress("DEPRECATION")
            MediaRecorder()
        }
        return try {
            r.setAudioSource(MediaRecorder.AudioSource.MIC)
            r.setOutputFormat(MediaRecorder.OutputFormat.MPEG_4)
            r.setAudioEncoder(MediaRecorder.AudioEncoder.AAC)
            r.setAudioChannels(1)
            r.setAudioSamplingRate(44_100)
            r.setAudioEncodingBitRate(48_000)
            r.setOutputFile(out.path)
            r.prepare()
            r.start()
            recorder = r
            file = out
            startedAt = SystemClock.elapsedRealtime()
            true
        } catch (e: Exception) {
            runCatching { r.release() }
            out.delete()
            false
        }
    }

    /** How loud right now (0–32767), for the recording animation. */
    fun level(): Int = runCatching { recorder?.maxAmplitude ?: 0 }.getOrDefault(0)

    /** Stops and returns the file (path, durationMs), or null if too short. */
    fun stop(): Map<String, Any>? {
        val r = recorder ?: return null
        val out = file ?: return null
        val duration = (SystemClock.elapsedRealtime() - startedAt).toInt()
        recorder = null
        file = null
        val ok = runCatching { r.stop() }.isSuccess
        runCatching { r.release() }
        if (!ok || duration < 500) {
            out.delete()
            return null
        }
        return mapOf("path" to out.path, "durationMs" to duration)
    }

    fun cancel() {
        val r = recorder ?: return
        recorder = null
        runCatching { r.stop() }
        runCatching { r.release() }
        file?.delete()
        file = null
    }
}
