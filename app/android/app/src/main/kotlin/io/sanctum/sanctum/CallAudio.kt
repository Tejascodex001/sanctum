package io.sanctum.sanctum

import android.content.Context
import android.media.AudioAttributes
import android.media.AudioDeviceInfo
import android.media.AudioFocusRequest
import android.media.AudioFormat
import android.media.AudioManager
import android.media.AudioRecord
import android.media.AudioTrack
import android.media.MediaCodec
import android.media.MediaCodecList
import android.media.MediaFormat
import android.media.MediaRecorder
import android.media.audiofx.AcousticEchoCanceler
import android.media.audiofx.AudioEffect
import android.media.audiofx.NoiseSuppressor
import android.os.Build
import android.os.PowerManager
import android.os.Process
import java.nio.ByteBuffer
import java.nio.ByteOrder

/**
 * A call's sound on this phone. One thread records from the microphone
 * (with the phone's echo canceller and noise suppressor when it has them),
 * encodes 20 ms frames (Opus, or AMR-WB where Opus can't be recorded) and
 * hands Rust a packet of two frames every 40 ms; Rust seals and sends it
 * (CallNative). Another thread takes what Rust's jitter buffer says to
 * play next, decodes it, fills gaps from lost packets with a fading copy
 * of the last sound, and plays it as a voice call. Nothing touches disk.
 *
 * Plays through the earpiece unless the speaker is on (a wired or Bluetooth
 * headset wins over the earpiece on Android 12+); near the ear, the screen
 * goes off.
 */
class CallAudio(private val context: Context) {
    @Volatile
    private var running = false

    @Volatile
    var muted = false

    private var recordThread: Thread? = null
    private var playThread: Thread? = null
    private val audio = context.getSystemService(Context.AUDIO_SERVICE) as AudioManager

    /** An AudioFocusRequest on Android 8+ (kept untyped for older phones). */
    private var focus: Any? = null
    private var proximity: PowerManager.WakeLock? = null

    fun start(send: String, recv: String, speaker: Boolean): Boolean {
        stop()
        if (!CallNative.ready()) return false
        running = true
        muted = false
        audio.mode = AudioManager.MODE_IN_COMMUNICATION
        requestFocus()
        setSpeaker(speaker)
        recordThread = Thread({ record(send) }, "sanctum-call-record").apply { start() }
        playThread = Thread({ play(recv) }, "sanctum-call-play").apply { start() }
        return true
    }

    fun stop() {
        running = false
        recordThread?.join(300)
        playThread?.join(300)
        recordThread = null
        playThread = null
        releaseProximity()
        abandonFocus()
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            runCatching { audio.clearCommunicationDevice() }
        } else {
            @Suppress("DEPRECATION")
            audio.isSpeakerphoneOn = false
        }
        audio.mode = AudioManager.MODE_NORMAL
    }

    fun setSpeaker(on: Boolean) {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            val wanted = if (on) {
                listOf(AudioDeviceInfo.TYPE_BUILTIN_SPEAKER)
            } else {
                listOf(
                    AudioDeviceInfo.TYPE_WIRED_HEADSET,
                    AudioDeviceInfo.TYPE_USB_HEADSET,
                    AudioDeviceInfo.TYPE_BLUETOOTH_SCO,
                    AudioDeviceInfo.TYPE_BLE_HEADSET,
                    AudioDeviceInfo.TYPE_BUILTIN_EARPIECE,
                )
            }
            val devices = audio.availableCommunicationDevices
            val device = wanted.firstNotNullOfOrNull { type -> devices.firstOrNull { it.type == type } }
            runCatching {
                if (device != null) audio.setCommunicationDevice(device) else audio.clearCommunicationDevice()
            }
        } else {
            @Suppress("DEPRECATION")
            audio.isSpeakerphoneOn = on
        }
        if (on) releaseProximity() else holdProximity()
    }

    private fun attributes(): AudioAttributes = AudioAttributes.Builder()
        .setUsage(AudioAttributes.USAGE_VOICE_COMMUNICATION)
        .setContentType(AudioAttributes.CONTENT_TYPE_SPEECH)
        .build()

    private fun requestFocus() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val request = AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN_TRANSIENT)
                .setAudioAttributes(attributes())
                .build()
            audio.requestAudioFocus(request)
            focus = request
        } else {
            @Suppress("DEPRECATION")
            audio.requestAudioFocus(null, AudioManager.STREAM_VOICE_CALL, AudioManager.AUDIOFOCUS_GAIN_TRANSIENT)
        }
    }

    private fun abandonFocus() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val request = focus
            if (request is AudioFocusRequest) audio.abandonAudioFocusRequest(request)
        } else {
            @Suppress("DEPRECATION")
            audio.abandonAudioFocus(null)
        }
        focus = null
    }

    /** Screen off near the ear (earpiece only). */
    private fun holdProximity() {
        if (proximity?.isHeld == true) return
        val power = context.getSystemService(Context.POWER_SERVICE) as PowerManager
        if (!power.isWakeLockLevelSupported(PowerManager.PROXIMITY_SCREEN_OFF_WAKE_LOCK)) return
        val lock = power.newWakeLock(PowerManager.PROXIMITY_SCREEN_OFF_WAKE_LOCK, "sanctum:call")
        lock.setReferenceCounted(false)
        runCatching { lock.acquire(4 * 60 * 60 * 1000L) }
        proximity = lock
    }

    private fun releaseProximity() {
        val lock = proximity ?: return
        proximity = null
        runCatching { if (lock.isHeld) lock.release(PowerManager.RELEASE_FLAG_WAIT_FOR_NO_PROXIMITY) }
    }

    // ---- Recording ----------------------------------------------------

    private fun record(codec: String) {
        Process.setThreadPriority(Process.THREAD_PRIORITY_URGENT_AUDIO)
        var recorder: AudioRecord? = null
        var rate = 0
        for (candidate in rates(codec)) {
            recorder = openRecorder(candidate)
            if (recorder != null) {
                rate = candidate
                break
            }
        }
        val rec = recorder ?: return
        val effects = attachEffects(rec.audioSessionId)
        val encoder = runCatching { openEncoder(codec, rate) }.getOrNull()
        if (encoder == null) {
            rec.release()
            effects.forEach { it.release() }
            return
        }
        val frameBytes = rate * FRAME_MS / 1000 * 2
        val pcm = ByteArray(frameBytes)
        val info = MediaCodec.BufferInfo()
        val frames = ArrayList<ByteArray>(FRAMES_PER_PACKET)
        var pts = 0L
        try {
            rec.startRecording()
            while (running) {
                var read = 0
                while (read < frameBytes && running) {
                    val n = rec.read(pcm, read, frameBytes - read)
                    if (n <= 0) break
                    read += n
                }
                if (!running) break
                if (read < frameBytes) {
                    Thread.sleep(10)
                    continue
                }
                // Muted: keep sending silence, so the call stays steady.
                if (muted) pcm.fill(0)
                feed(encoder, pcm, pcm.size, pts)
                pts += FRAME_MS * 1000L
                drain(encoder, info) { frame ->
                    frames.add(frame)
                    if (frames.size == FRAMES_PER_PACKET) {
                        CallNative.send(pack(frames))
                        frames.clear()
                    }
                }
            }
        } catch (e: Exception) {
            // The microphone went away (or another app took it): stop quietly.
        } finally {
            runCatching { rec.stop() }
            rec.release()
            effects.forEach { runCatching { it.release() } }
            runCatching { encoder.stop() }
            encoder.release()
        }
    }

    private fun openRecorder(rate: Int): AudioRecord? {
        val minBuf = AudioRecord.getMinBufferSize(rate, AudioFormat.CHANNEL_IN_MONO, AudioFormat.ENCODING_PCM_16BIT)
        if (minBuf <= 0) return null
        val frameBytes = rate * FRAME_MS / 1000 * 2
        val recorder = try {
            AudioRecord(
                MediaRecorder.AudioSource.VOICE_COMMUNICATION,
                rate,
                AudioFormat.CHANNEL_IN_MONO,
                AudioFormat.ENCODING_PCM_16BIT,
                maxOf(minBuf, frameBytes * 4),
            )
        } catch (e: Exception) {
            return null
        }
        if (recorder.state != AudioRecord.STATE_INITIALIZED) {
            recorder.release()
            return null
        }
        return recorder
    }

    private fun attachEffects(session: Int): List<AudioEffect> {
        val effects = ArrayList<AudioEffect>()
        runCatching {
            if (AcousticEchoCanceler.isAvailable()) {
                AcousticEchoCanceler.create(session)?.let {
                    it.setEnabled(true)
                    effects.add(it)
                }
            }
        }
        runCatching {
            if (NoiseSuppressor.isAvailable()) {
                NoiseSuppressor.create(session)?.let {
                    it.setEnabled(true)
                    effects.add(it)
                }
            }
        }
        return effects
    }

    private fun openEncoder(codec: String, rate: Int): MediaCodec {
        val format = MediaFormat.createAudioFormat(mime(codec), rate, 1)
        format.setInteger(MediaFormat.KEY_BIT_RATE, if (codec == OPUS) 20_000 else 23_850)
        format.setInteger(MediaFormat.KEY_MAX_INPUT_SIZE, rate * FRAME_MS / 1000 * 2 * 2)
        val encoder = MediaCodec.createEncoderByType(mime(codec))
        try {
            encoder.configure(format, null, null, MediaCodec.CONFIGURE_FLAG_ENCODE)
            encoder.start()
        } catch (e: Exception) {
            encoder.release()
            throw e
        }
        return encoder
    }

    // ---- Playing ------------------------------------------------------

    private fun play(codec: String) {
        Process.setThreadPriority(Process.THREAD_PRIORITY_URGENT_AUDIO)
        val rate = playRate(codec)
        val packetBytes = rate * FRAME_MS * FRAMES_PER_PACKET / 1000 * 2
        val minBuf = AudioTrack.getMinBufferSize(rate, AudioFormat.CHANNEL_OUT_MONO, AudioFormat.ENCODING_PCM_16BIT)
        val track = try {
            AudioTrack.Builder()
                .setAudioAttributes(attributes())
                .setAudioFormat(
                    AudioFormat.Builder()
                        .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                        .setSampleRate(rate)
                        .setChannelMask(AudioFormat.CHANNEL_OUT_MONO)
                        .build(),
                )
                .setBufferSizeInBytes(maxOf(minBuf, packetBytes * 2))
                .setTransferMode(AudioTrack.MODE_STREAM)
                .build()
        } catch (e: Exception) {
            return
        }
        val decoder = runCatching { openDecoder(codec, rate) }.getOrNull()
        if (decoder == null) {
            track.release()
            return
        }
        val info = MediaCodec.BufferInfo()
        val pending = PcmQueue()
        val conceal = Concealer(packetBytes)
        var pts = 0L
        try {
            track.play()
            while (running) {
                var tries = 0
                var real = true
                while (pending.size < packetBytes && tries < 3) {
                    tries++
                    val pulled = CallNative.pull()
                    when {
                        pulled == null -> {
                            pending.add(ByteArray(packetBytes))
                            real = false
                        }
                        pulled.isEmpty() -> {
                            pending.add(conceal.fill())
                            real = false
                        }
                        else -> {
                            for (frame in unpack(pulled)) {
                                feed(decoder, frame, frame.size, pts)
                                pts += FRAME_MS * 1000L
                            }
                            drain(decoder, info) { pending.add(it) }
                        }
                    }
                }
                if (pending.size < packetBytes) pending.add(ByteArray(packetBytes - pending.size))
                // Never let decoded sound pile up: that's only delay.
                pending.trim(packetBytes * 4)
                val chunk = pending.take(packetBytes)
                if (real) conceal.heard(chunk)
                // Blocks while the track is full: this paces the loop.
                track.write(chunk, 0, chunk.size)
            }
        } catch (e: Exception) {
            // The output went away: stop quietly.
        } finally {
            runCatching { track.stop() }
            track.release()
            runCatching { decoder.stop() }
            decoder.release()
        }
    }

    private fun openDecoder(codec: String, rate: Int): MediaCodec {
        val format = MediaFormat.createAudioFormat(mime(codec), rate, 1)
        if (codec == OPUS) {
            format.setByteBuffer("csd-0", ByteBuffer.wrap(opusHead()))
            format.setByteBuffer("csd-1", ByteBuffer.wrap(nanos(0L)))
            format.setByteBuffer("csd-2", ByteBuffer.wrap(nanos(80_000_000L)))
        }
        val decoder = MediaCodec.createDecoderByType(mime(codec))
        try {
            decoder.configure(format, null, null, 0)
            decoder.start()
        } catch (e: Exception) {
            decoder.release()
            throw e
        }
        return decoder
    }

    private fun feed(codec: MediaCodec, data: ByteArray, length: Int, pts: Long) {
        val index = codec.dequeueInputBuffer(10_000)
        // The codec is behind: drop this frame rather than wait.
        if (index < 0) return
        val buffer = codec.getInputBuffer(index)
        if (buffer == null) {
            codec.queueInputBuffer(index, 0, 0, pts, 0)
            return
        }
        buffer.clear()
        val n = minOf(length, buffer.remaining())
        buffer.put(data, 0, n)
        codec.queueInputBuffer(index, 0, n, pts, 0)
    }

    private fun drain(codec: MediaCodec, info: MediaCodec.BufferInfo, out: (ByteArray) -> Unit) {
        while (true) {
            val index = codec.dequeueOutputBuffer(info, 0)
            if (index == MediaCodec.INFO_TRY_AGAIN_LATER) return
            if (index < 0) continue // format or buffers changed
            val buffer = codec.getOutputBuffer(index)
            val config = (info.flags and MediaCodec.BUFFER_FLAG_CODEC_CONFIG) != 0
            if (buffer != null && info.size > 0 && !config) {
                val bytes = ByteArray(info.size)
                buffer.position(info.offset)
                buffer.limit(info.offset + info.size)
                buffer.get(bytes)
                out(bytes)
            }
            codec.releaseOutputBuffer(index, false)
        }
    }

    /** Decoded sound waiting to be played. */
    private class PcmQueue {
        private var data = ByteArray(0)
        val size: Int get() = data.size

        fun add(bytes: ByteArray) {
            data += bytes
        }

        fun take(n: Int): ByteArray {
            val out = data.copyOfRange(0, n)
            data = data.copyOfRange(n, data.size)
            return out
        }

        fun trim(max: Int) {
            if (data.size > max) data = data.copyOfRange(data.size - max, data.size)
        }
    }

    /** Fills a lost packet with the last sound, fading out. */
    private class Concealer(private val packetBytes: Int) {
        private var last = ByteArray(packetBytes)
        private var gain = 1f

        fun heard(chunk: ByteArray) {
            if (chunk.size != packetBytes) return
            last = chunk
            gain = 1f
        }

        fun fill(): ByteArray {
            gain *= 0.5f
            val out = ByteArray(packetBytes)
            if (gain < 0.1f) return out
            val src = ByteBuffer.wrap(last).order(ByteOrder.LITTLE_ENDIAN)
            val dst = ByteBuffer.wrap(out).order(ByteOrder.LITTLE_ENDIAN)
            while (src.remaining() >= 2) dst.putShort((src.getShort() * gain).toInt().toShort())
            return out
        }
    }

    companion object {
        const val OPUS = "opus"
        const val AMR_WB = "amr-wb"
        private const val FRAME_MS = 20
        private const val FRAMES_PER_PACKET = 2

        private fun mime(codec: String) = if (codec == OPUS) "audio/opus" else "audio/amr-wb"

        /** Recording rates to try: Opus takes 48 or 16 kHz, AMR-WB 16. */
        private fun rates(codec: String) = if (codec == OPUS) listOf(48_000, 16_000) else listOf(16_000)

        private fun playRate(codec: String) = if (codec == OPUS) 48_000 else 16_000

        private fun has(mime: String, encoder: Boolean): Boolean = runCatching {
            MediaCodecList(MediaCodecList.REGULAR_CODECS).codecInfos.any { info ->
                info.isEncoder == encoder && info.supportedTypes.any { it.equals(mime, ignoreCase = true) }
            }
        }.getOrDefault(false)

        /** What this phone can play and record into, best first. */
        fun codecs(): Map<String, List<String>> {
            val all = listOf(OPUS, AMR_WB)
            val play = all.filter { has(mime(it), encoder = false) }
            // Android records Opus from Android 10 on.
            val record = all.filter {
                has(mime(it), encoder = true) && (it != OPUS || Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q)
            }
            return mapOf("play" to play, "record" to record)
        }

        /** Opus's identification header, which Android's decoder needs. */
        private fun opusHead(): ByteArray = ByteBuffer.allocate(19)
            .order(ByteOrder.LITTLE_ENDIAN)
            .put("OpusHead".toByteArray(Charsets.US_ASCII))
            .put(1.toByte()) // version
            .put(1.toByte()) // mono
            .putShort(0.toShort()) // pre-skip
            .putInt(48_000) // original rate
            .putShort(0.toShort()) // gain
            .put(0.toByte()) // mapping family
            .array()

        private fun nanos(value: Long): ByteArray =
            ByteBuffer.allocate(8).order(ByteOrder.nativeOrder()).putLong(value).array()

        /** Two codec frames in one packet: a count, then each with its length. */
        fun pack(frames: List<ByteArray>): ByteArray {
            val size = 1 + frames.sumOf { 2 + it.size }
            val out = ByteBuffer.allocate(size)
            out.put(frames.size.toByte())
            for (f in frames) {
                out.putShort(f.size.toShort())
                out.put(f)
            }
            return out.array()
        }

        fun unpack(packet: ByteArray): List<ByteArray> {
            val frames = ArrayList<ByteArray>()
            val buffer = ByteBuffer.wrap(packet)
            if (!buffer.hasRemaining()) return frames
            val count = buffer.get().toInt() and 0xFF
            repeat(count) {
                if (buffer.remaining() < 2) return frames
                val length = buffer.getShort().toInt() and 0xFFFF
                if (buffer.remaining() < length) return frames
                val frame = ByteArray(length)
                buffer.get(frame)
                frames.add(frame)
            }
            return frames
        }
    }
}
