package io.sanctum.sanctum

import android.annotation.SuppressLint
import android.content.Context
import android.hardware.camera2.CameraCaptureSession
import android.hardware.camera2.CameraCharacteristics
import android.hardware.camera2.CameraDevice
import android.hardware.camera2.CameraManager
import android.hardware.camera2.CaptureRequest
import android.hardware.display.DisplayManager
import android.media.MediaCodec
import android.media.MediaCodecInfo
import android.media.MediaCodecList
import android.media.MediaFormat
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.HandlerThread
import android.os.Looper
import android.util.Range
import android.util.Size
import android.view.Display
import android.view.OrientationEventListener
import android.view.Surface
import io.flutter.view.TextureRegistry

/**
 * A call's pictures on this phone.
 *
 * Sending: the camera (Camera2) draws into a hardware encoder's input
 * surface (H.264, or VP8 where that's all the phone records) and into a
 * small preview; each encoded frame goes to Rust (CallNative.sendVideo),
 * which splits, seals and sends it. Rust says how sharp to be and when a
 * keyframe is needed (CallNative.videoTarget): low quality through Tor,
 * higher over nearby or direct paths.
 *
 * Receiving: frames come from Rust when their sound is playing
 * (CallNative.pullVideo) and are decoded straight onto a Flutter texture,
 * which Dart shows with `Texture(textureId)`. Nothing touches disk.
 *
 * Frames from the camera are sideways (as the sensor is mounted); each
 * carries how far to turn it, and the screen showing it does the turning.
 */
class CallVideo(
    private val context: Context,
    private val textures: TextureRegistry,
    /** A picture's shape changed: (which, width, height, rotation, mirror). */
    private val onFormat: (String, Int, Int, Int, Boolean) -> Unit,
) {
    private val main = Handler(Looper.getMainLooper())

    @Volatile
    private var local: TextureRegistry.SurfaceProducer? = null

    @Volatile
    private var remote: TextureRegistry.SurfaceProducer? = null

    @Volatile
    private var open = false

    @Volatile
    private var remoteSurfaceChanged = false
    private var decodeThread: Thread? = null
    private var recvCodec = H264
    private var sendCodec = H264

    // Camera and encoder, used on the camera thread.
    private var cameraThread: HandlerThread? = null
    private var cameraHandler: Handler? = null
    private var device: CameraDevice? = null
    private var session: CameraCaptureSession? = null
    private var encoder: MediaCodec? = null
    private var encoderSurface: Surface? = null
    private var encodeThread: Thread? = null

    @Volatile
    private var capturing = false
    private var front = true
    private var sensorOrientation = 0
    private var size = Size(320, 240)
    private var quality = intArrayOf(0, 0, 0)

    @Volatile
    private var deviceOrientation = 0
    private var orientation: OrientationEventListener? = null

    /** Opens the two pictures (Flutter textures); returns their ids. */
    fun open(send: String, recv: String): Map<String, Long> {
        close()
        sendCodec = send
        recvCodec = recv
        val mine = textures.createSurfaceProducer()
        val theirs = textures.createSurfaceProducer()
        theirs.setCallback(object : TextureRegistry.SurfaceProducer.Callback {
            override fun onSurfaceAvailable() {
                remoteSurfaceChanged = true
            }

            override fun onSurfaceCleanup() {
                remoteSurfaceChanged = true
            }
        })
        local = mine
        remote = theirs
        open = true
        if (recv.isNotEmpty()) {
            decodeThread = Thread({ decodeLoop() }, "sanctum-video-recv").apply { start() }
        }
        return mapOf("local" to mine.id(), "remote" to theirs.id())
    }

    /** Stops everything and gives the textures back. */
    fun close() {
        stopCamera()
        open = false
        decodeThread?.join(500)
        decodeThread = null
        local?.release()
        remote?.release()
        local = null
        remote = null
    }

    // ---- Sending ----------------------------------------------------

    /** Turns the camera on (front or back) or off. False if it can't start. */
    fun camera(on: Boolean, useFront: Boolean): Boolean {
        if (!on) {
            stopCamera()
            return true
        }
        if (!open || sendCodec.isEmpty()) return false
        if (capturing && useFront == front) return true
        stopCamera()
        front = useFront
        val thread = HandlerThread("sanctum-camera").apply { start() }
        cameraThread = thread
        val handler = Handler(thread.looper)
        cameraHandler = handler
        capturing = true
        watchOrientation()
        handler.post { if (!openCamera()) giveUp() }
        return true
    }

    private fun stopCamera() {
        capturing = false
        orientation?.disable()
        orientation = null
        val handler = cameraHandler
        val thread = cameraThread
        cameraHandler = null
        cameraThread = null
        if (handler != null && thread != null) {
            // Close on the camera thread, then let it end.
            handler.post { closeCamera() }
            thread.quitSafely()
            thread.join(1000)
        }
        encodeThread?.join(500)
        encodeThread = null
    }

    private fun closeCamera() {
        runCatching { session?.close() }
        session = null
        runCatching { device?.close() }
        device = null
        runCatching { encoder?.stop() }
        runCatching { encoder?.release() }
        encoder = null
        runCatching { encoderSurface?.release() }
        encoderSurface = null
    }

    /** Which way the phone is held, for turning the pictures we send. */
    private fun watchOrientation() {
        val listener = object : OrientationEventListener(context) {
            override fun onOrientationChanged(degrees: Int) {
                if (degrees == ORIENTATION_UNKNOWN) return
                deviceOrientation = ((degrees + 45) / 90 % 4) * 90
            }
        }
        if (listener.canDetectOrientation()) listener.enable()
        orientation = listener
    }

    private fun cameraId(manager: CameraManager): String? {
        val wanted = if (front) CameraCharacteristics.LENS_FACING_FRONT else CameraCharacteristics.LENS_FACING_BACK
        val ids = manager.cameraIdList
        return ids.firstOrNull { manager.getCameraCharacteristics(it).get(CameraCharacteristics.LENS_FACING) == wanted }
            ?: ids.firstOrNull()
    }

    @SuppressLint("MissingPermission") // asked for before the call screen turns the camera on
    private fun openCamera(): Boolean {
        val handler = cameraHandler ?: return false
        val manager = context.getSystemService(Context.CAMERA_SERVICE) as CameraManager
        val id = runCatching { cameraId(manager) }.getOrNull() ?: return false
        val chars = runCatching { manager.getCameraCharacteristics(id) }.getOrNull() ?: return false
        sensorOrientation = chars.get(CameraCharacteristics.SENSOR_ORIENTATION) ?: 0
        val target = CallNative.videoTarget() ?: intArrayOf(180_000, 12, 240, 1)
        quality = intArrayOf(target[0], target[1], target[2])
        val map = chars.get(CameraCharacteristics.SCALER_STREAM_CONFIGURATION_MAP) ?: return false
        val sizes = map.getOutputSizes(MediaCodec::class.java) ?: return false
        size = pickSize(sizes, target[2])
        val fpsRange = pickFps(chars.get(CameraCharacteristics.CONTROL_AE_AVAILABLE_TARGET_FPS_RANGES), target[1])
        val producer = local ?: return false
        producer.setSize(size.width, size.height)
        val preview = producer.surface
        reportLocal(producer)
        val codec = openEncoder(size, target[0], target[1]) ?: return false
        val input = codec.second
        encoder = codec.first
        encoderSurface = input
        encodeThread = Thread({ encodeLoop(codec.first) }, "sanctum-video-send").apply { start() }
        return try {
            manager.openCamera(id, object : CameraDevice.StateCallback() {
                override fun onOpened(camera: CameraDevice) {
                    if (!capturing) {
                        camera.close()
                        return
                    }
                    device = camera
                    startSession(camera, preview, input, fpsRange)
                }

                override fun onDisconnected(camera: CameraDevice) {
                    camera.close()
                    if (device == camera) device = null
                }

                override fun onError(camera: CameraDevice, error: Int) {
                    camera.close()
                    if (device == camera) device = null
                }
            }, handler)
            true
        } catch (e: Exception) {
            false
        }
    }

    @Suppress("DEPRECATION") // the list form works on every Android version we support
    private fun startSession(camera: CameraDevice, preview: Surface, input: Surface, fps: Range<Int>?) {
        val handler = cameraHandler ?: return
        try {
            camera.createCaptureSession(listOf(preview, input), object : CameraCaptureSession.StateCallback() {
                override fun onConfigured(s: CameraCaptureSession) {
                    if (!capturing) {
                        s.close()
                        return
                    }
                    session = s
                    runCatching {
                        val request = camera.createCaptureRequest(CameraDevice.TEMPLATE_RECORD)
                        request.addTarget(preview)
                        request.addTarget(input)
                        request.set(CaptureRequest.CONTROL_MODE, CaptureRequest.CONTROL_MODE_AUTO)
                        if (fps != null) request.set(CaptureRequest.CONTROL_AE_TARGET_FPS_RANGE, fps)
                        s.setRepeatingRequest(request.build(), null, handler)
                    }
                }

                override fun onConfigureFailed(s: CameraCaptureSession) {
                    s.close()
                }
            }, handler)
        } catch (e: Exception) {
            // The camera went away meanwhile.
        }
    }

    private fun openEncoder(size: Size, bitrate: Int, fps: Int): Pair<MediaCodec, Surface>? {
        val mime = mime(sendCodec)
        for (baseline in listOf(true, false)) {
            val format = MediaFormat.createVideoFormat(mime, size.width, size.height)
            format.setInteger(MediaFormat.KEY_COLOR_FORMAT, MediaCodecInfo.CodecCapabilities.COLOR_FormatSurface)
            format.setInteger(MediaFormat.KEY_BIT_RATE, bitrate)
            format.setInteger(MediaFormat.KEY_FRAME_RATE, fps)
            // Keyframes come when asked for (after a loss); now and then
            // anyway, so a picture never stays broken for long.
            format.setInteger(MediaFormat.KEY_I_FRAME_INTERVAL, 4)
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                format.setFloat(MediaFormat.KEY_MAX_FPS_TO_ENCODER, fps.toFloat())
            }
            if (baseline && mime == MediaFormat.MIMETYPE_VIDEO_AVC) {
                format.setInteger(MediaFormat.KEY_PROFILE, MediaCodecInfo.CodecProfileLevel.AVCProfileBaseline)
                format.setInteger(MediaFormat.KEY_LEVEL, MediaCodecInfo.CodecProfileLevel.AVCLevel31)
            }
            val codec = runCatching { MediaCodec.createEncoderByType(mime) }.getOrNull() ?: return null
            try {
                codec.configure(format, null, null, MediaCodec.CONFIGURE_FLAG_ENCODE)
                val surface = codec.createInputSurface()
                codec.start()
                return Pair(codec, surface)
            } catch (e: Exception) {
                codec.release()
            }
        }
        return null
    }

    /** Hands each encoded frame to Rust, and follows what Rust asks for. */
    private fun encodeLoop(codec: MediaCodec) {
        val info = MediaCodec.BufferInfo()
        var config = ByteArray(0)
        var lastKey = 0L
        try {
            while (capturing) {
                // What Rust wants now: sharpness, and a keyframe after a loss.
                val target = CallNative.videoTarget()
                if (target != null) {
                    if (target[3] == 1 && System.currentTimeMillis() - lastKey > 500) {
                        lastKey = System.currentTimeMillis()
                        val sync = Bundle()
                        sync.putInt(MediaCodec.PARAMETER_KEY_REQUEST_SYNC_FRAME, 0)
                        runCatching { codec.setParameters(sync) }
                    }
                    if (target[1] != quality[1] || target[2] != quality[2]) {
                        // A different path (e.g. a direct one came up):
                        // start again at the size and speed it allows.
                        quality = intArrayOf(target[0], target[1], target[2])
                        cameraHandler?.post { restartCamera() }
                        return
                    }
                    if (Math.abs(target[0] - quality[0]) > quality[0] / 10) {
                        quality[0] = target[0]
                        val rate = Bundle()
                        rate.putInt(MediaCodec.PARAMETER_KEY_VIDEO_BITRATE, target[0])
                        runCatching { codec.setParameters(rate) }
                    }
                }
                val index = codec.dequeueOutputBuffer(info, 20_000)
                if (index < 0) continue
                val buffer = codec.getOutputBuffer(index)
                if (buffer != null && info.size > 0) {
                    val bytes = ByteArray(info.size)
                    buffer.position(info.offset)
                    buffer.limit(info.offset + info.size)
                    buffer.get(bytes)
                    if ((info.flags and MediaCodec.BUFFER_FLAG_CODEC_CONFIG) != 0) {
                        // H.264's parameter sets: sent with every keyframe, so
                        // the friend can start (or start again) from any.
                        config = bytes
                    } else {
                        val key = (info.flags and MediaCodec.BUFFER_FLAG_KEY_FRAME) != 0
                        val frame = if (key && config.isNotEmpty()) config + bytes else bytes
                        CallNative.sendVideo(frame, key, sendRotation(), size.width, size.height)
                    }
                }
                codec.releaseOutputBuffer(index, false)
            }
        } catch (e: Exception) {
            // The encoder went away (the camera stopped): done.
        }
    }

    /** On the camera thread: the same camera again, at the new quality. */
    private fun restartCamera() {
        if (!capturing) return
        closeCamera()
        encodeThread = null
        if (!openCamera()) giveUp()
    }

    /** On the camera thread: the camera couldn't start; tidy up (the thread ends with [stopCamera]). */
    private fun giveUp() {
        capturing = false
        closeCamera()
    }

    /** How far the friend turns our picture: the sensor's mounting and how the phone is held. */
    private fun sendRotation(): Int = if (front) {
        (sensorOrientation + deviceOrientation) % 360
    } else {
        (sensorOrientation - deviceOrientation + 360) % 360
    }

    /** Tells Dart how to show our own picture. */
    private fun reportLocal(producer: TextureRegistry.SurfaceProducer) {
        val display = (context.getSystemService(Context.DISPLAY_SERVICE) as DisplayManager).getDisplay(Display.DEFAULT_DISPLAY)
        val displayDegrees = when (display?.rotation) {
            Surface.ROTATION_90 -> 90
            Surface.ROTATION_180 -> 180
            Surface.ROTATION_270 -> 270
            else -> 0
        }
        val (w, h) = Pair(size.width, size.height)
        if (producer.handlesCropAndRotation()) {
            // Android already turned (and, for the front camera, mirrored) it.
            val sideways = sensorOrientation % 180 == 90
            val rotation = (360 - displayDegrees) % 360
            main.post { onFormat("local", if (sideways) h else w, if (sideways) w else h, rotation, false) }
        } else {
            val rotation = if (front) {
                (sensorOrientation + displayDegrees) % 360
            } else {
                (sensorOrientation - displayDegrees + 360) % 360
            }
            main.post { onFormat("local", w, h, rotation, front) }
        }
    }

    // ---- Receiving --------------------------------------------------

    private fun decodeLoop() {
        var decoder: MediaCodec? = null
        var width = 0
        var height = 0
        var turns = -1
        var pts = 0L
        val info = MediaCodec.BufferInfo()
        try {
            while (open) {
                val frame = CallNative.pullVideo()
                if (frame == null || frame.size <= HEADER) {
                    if (decoder != null) render(decoder, info)
                    Thread.sleep(5)
                    continue
                }
                val key = frame[0].toInt() == 1
                val quarter = frame[1].toInt() and 3
                val w = ((frame[2].toInt() and 0xFF) shl 8) or (frame[3].toInt() and 0xFF)
                val h = ((frame[4].toInt() and 0xFF) shl 8) or (frame[5].toInt() and 0xFF)
                if (decoder != null && (remoteSurfaceChanged || (key && (w != width || h != height)))) {
                    release(decoder)
                    decoder = null
                    if (remoteSurfaceChanged) CallNative.askKeyframe()
                }
                if (decoder == null) {
                    // Only a keyframe can start a picture.
                    if (!key || w <= 0 || h <= 0) continue
                    remoteSurfaceChanged = false
                    decoder = openDecoder(w, h)
                    if (decoder == null) {
                        Thread.sleep(200)
                        continue
                    }
                    width = w
                    height = h
                    turns = -1
                }
                if (quarter != turns) {
                    turns = quarter
                    main.post { onFormat("remote", w, h, quarter * 90, false) }
                }
                if (!feed(decoder, frame, key, pts)) {
                    // The decoder fell over: start again at the next keyframe.
                    release(decoder)
                    decoder = null
                    CallNative.askKeyframe()
                    continue
                }
                pts += 33_333
                render(decoder, info)
            }
        } catch (e: InterruptedException) {
            // Closing.
        } finally {
            val last = decoder
            if (last != null) release(last)
        }
    }

    private fun release(codec: MediaCodec) {
        try {
            codec.stop()
        } catch (e: Exception) {
            // Already stopped.
        }
        codec.release()
    }

    private fun openDecoder(w: Int, h: Int): MediaCodec? {
        val producer = remote ?: return null
        producer.setSize(w, h)
        val surface = producer.surface
        val codec = runCatching { MediaCodec.createDecoderByType(mime(recvCodec)) }.getOrNull() ?: return null
        return try {
            val format = MediaFormat.createVideoFormat(mime(recvCodec), w, h)
            codec.configure(format, surface, null, 0)
            codec.start()
            codec
        } catch (e: Exception) {
            codec.release()
            null
        }
    }

    /** Feeds one frame (after its header); false if the decoder failed. */
    private fun feed(codec: MediaCodec, frame: ByteArray, key: Boolean, pts: Long): Boolean {
        return try {
            var start = HEADER
            if (key && recvCodec == H264) {
                // The parameter sets go in first, marked as such.
                val split = keyNalStart(frame, HEADER)
                if (split > HEADER) {
                    if (!queue(codec, frame, HEADER, split - HEADER, 0, MediaCodec.BUFFER_FLAG_CODEC_CONFIG)) return true
                    start = split
                }
            }
            queue(codec, frame, start, frame.size - start, pts, if (key) MediaCodec.BUFFER_FLAG_KEY_FRAME else 0)
            true
        } catch (e: Exception) {
            false
        }
    }

    /** Queues input; false if the decoder had no room (the frame is dropped). */
    private fun queue(codec: MediaCodec, data: ByteArray, offset: Int, length: Int, pts: Long, flags: Int): Boolean {
        val index = codec.dequeueInputBuffer(20_000)
        if (index < 0) return false
        val buffer = codec.getInputBuffer(index) ?: return false
        buffer.clear()
        if (buffer.remaining() < length) {
            codec.queueInputBuffer(index, 0, 0, pts, 0)
            return false
        }
        buffer.put(data, offset, length)
        codec.queueInputBuffer(index, 0, length, pts, flags)
        return true
    }

    /** Shows every decoded frame that's ready. */
    private fun render(codec: MediaCodec, info: MediaCodec.BufferInfo) {
        try {
            while (true) {
                val index = codec.dequeueOutputBuffer(info, 0)
                if (index == MediaCodec.INFO_TRY_AGAIN_LATER) return
                if (index < 0) continue // format or buffers changed
                codec.releaseOutputBuffer(index, info.size > 0)
            }
        } catch (e: Exception) {
            // Reported by the next feed.
        }
    }

    companion object {
        const val H264 = "h264"
        const val VP8 = "vp8"

        /** Bytes Rust puts before each frame (see call_jni.rs). */
        private const val HEADER = 6

        private fun mime(codec: String) =
            if (codec == VP8) MediaFormat.MIMETYPE_VIDEO_VP8 else MediaFormat.MIMETYPE_VIDEO_AVC

        private fun has(mime: String, encoder: Boolean): Boolean = runCatching {
            MediaCodecList(MediaCodecList.REGULAR_CODECS).codecInfos.any { info ->
                info.isEncoder == encoder && info.supportedTypes.any { it.equals(mime, ignoreCase = true) }
            }
        }.getOrDefault(false)

        /** What this phone can play and record into, best first. */
        fun codecs(camera: Boolean): Map<String, List<String>> {
            val all = listOf(H264, VP8)
            val play = all.filter { has(mime(it), encoder = false) }
            val record = if (camera) all.filter { has(mime(it), encoder = true) } else emptyList()
            return mapOf("videoPlay" to play, "videoRecord" to record)
        }

        /** The camera size closest to `height` lines (4:3 preferred). */
        fun pickSize(sizes: Array<Size>, height: Int): Size {
            val big = sizes.filter { minOf(it.width, it.height) >= height }
            val pool = big.ifEmpty { sizes.toList() }
            val fourThree = pool.filter { maxOf(it.width, it.height) * 3 == minOf(it.width, it.height) * 4 }
            return fourThree.ifEmpty { pool }.minByOrNull { it.width * it.height } ?: Size(320, 240)
        }

        /** The camera's frame-rate range that best fits `fps`. */
        fun pickFps(ranges: Array<Range<Int>>?, fps: Int): Range<Int>? {
            if (ranges.isNullOrEmpty()) return null
            val fitting = ranges.filter { it.upper >= fps }
            return fitting.minWithOrNull(compareBy<Range<Int>>({ it.upper }, { -it.lower })) ?: ranges.maxByOrNull { it.upper }
        }

        /** Where the picture itself starts in an H.264 keyframe (after its parameter sets). */
        fun keyNalStart(data: ByteArray, from: Int): Int {
            var i = from
            while (i + 3 < data.size) {
                val three = data[i].toInt() == 0 && data[i + 1].toInt() == 0 && data[i + 2].toInt() == 1
                val four = i + 4 < data.size && data[i].toInt() == 0 && data[i + 1].toInt() == 0 &&
                    data[i + 2].toInt() == 0 && data[i + 3].toInt() == 1
                if (three || four) {
                    val header = if (four) i + 4 else i + 3
                    val type = data[header].toInt() and 0x1F
                    // A slice (1) or an IDR slice (5): the picture.
                    if (type == 1 || type == 5) return i
                    i = header
                } else {
                    i++
                }
            }
            return from
        }
    }
}
