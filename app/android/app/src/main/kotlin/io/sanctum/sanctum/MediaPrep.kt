package io.sanctum.sanctum

import android.content.ContentResolver
import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.ImageDecoder
import android.graphics.Matrix
import android.media.ExifInterface
import android.media.MediaMetadataRetriever
import android.net.Uri
import android.os.Build
import android.provider.OpenableColumns
import android.webkit.MimeTypeMap
import java.io.ByteArrayOutputStream
import java.io.File
import kotlin.math.max
import kotlin.math.roundToInt

/**
 * Gets a picked photo, video or file ready to send: copied out of the
 * picker, photos shrunk and re-encoded as JPEG (which also drops hidden
 * metadata such as GPS location), and a tiny preview made. The Rust side
 * then encrypts it.
 */
object MediaPrep {
    /** Long side of a sent photo: sharp on a phone, quick over Tor. */
    private const val MAX_SIDE = 2048
    private const val THUMB_SIDE = 64

    /**
     * The file type to tell other apps. A picker often reports nothing
     * useful (or "any file"), and then no app would match, so the ending of
     * the file name decides.
     */
    fun guessMime(given: String?, name: String): String {
        val type = given?.trim()?.lowercase().orEmpty()
        if (type.isNotEmpty() && type != "*/*" && type != "application/octet-stream" && type != "binary/octet-stream") {
            return type
        }
        val ext = name.substringAfterLast('.', "").lowercase()
        if (ext.isEmpty()) return "application/octet-stream"
        return MimeTypeMap.getSingleton().getMimeTypeFromExtension(ext)
            ?: when (ext) {
                // Some older phones don't know these.
                "heic", "heif" -> "image/heic"
                "avif" -> "image/avif"
                "webp" -> "image/webp"
                "m4a" -> "audio/mp4"
                "opus", "ogg", "oga" -> "audio/ogg"
                "mkv" -> "video/x-matroska"
                "webm" -> "video/webm"
                "3gp" -> "video/3gpp"
                else -> "application/octet-stream"
            }
    }

    /** A file name for saving: no slashes, and an ending that matches [mime]. */
    fun saveName(name: String, mime: String): String {
        val clean = name.replace('/', '_').replace('\\', '_').trim().ifEmpty { "file" }
        if (clean.substringAfterLast('.', "").isNotEmpty()) return clean
        val ext = MimeTypeMap.getSingleton().getExtensionFromMimeType(mime) ?: return clean
        return "$clean.$ext"
    }

    fun prepare(context: Context, uri: Uri): Map<String, Any> {
        val resolver = context.contentResolver
        val name = displayName(resolver, uri) ?: "file"
        val mime = guessMime(resolver.getType(uri), name)
        val dir = File(context.cacheDir, "outgoing").apply { mkdirs() }
        val raw = File(dir, "${System.nanoTime()}-${name.replace(Regex("[^A-Za-z0-9._-]"), "_").takeLast(80)}")
        resolver.openInputStream(uri)!!.use { input -> raw.outputStream().use { input.copyTo(it) } }
        return when {
            mime.startsWith("image/") && mime != "image/gif" -> image(raw, name) ?: file(raw, name, mime)
            mime.startsWith("video/") -> video(raw, name, mime)
            else -> file(raw, name, mime)
        }
    }

    private fun file(raw: File, name: String, mime: String, kind: String = "file") = mapOf(
        "path" to raw.path,
        "name" to name,
        "mime" to mime,
        "kind" to kind,
        "width" to 0,
        "height" to 0,
        "durationMs" to 0,
        "thumb" to ByteArray(0),
    )

    /**
     * Photo formats BitmapFactory can't read (HEIC and AVIF on some phones)
     * but the newer ImageDecoder can. The result is already upright.
     */
    private fun decodeWithImageDecoder(raw: File): Bitmap? {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.P) return null
        return runCatching {
            ImageDecoder.decodeBitmap(ImageDecoder.createSource(raw)) { decoder, info, _ ->
                decoder.allocator = ImageDecoder.ALLOCATOR_SOFTWARE
                val longest = max(info.size.width, info.size.height)
                if (longest > MAX_SIDE) {
                    val scale = MAX_SIDE.toFloat() / longest
                    decoder.setTargetSize(
                        (info.size.width * scale).roundToInt().coerceAtLeast(1),
                        (info.size.height * scale).roundToInt().coerceAtLeast(1),
                    )
                }
            }
        }.getOrNull()
    }

    /**
     * The photo shrunk and saved as JPEG, or null if this phone can't read
     * it (it is then sent as a plain file, which other phones can still save).
     */
    private fun image(raw: File, name: String): Map<String, Any>? {
        val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
        BitmapFactory.decodeFile(raw.path, bounds)
        val upright: Bitmap = run {
            if (bounds.outWidth > 0 && bounds.outHeight > 0) {
                var sample = 1
                while (max(bounds.outWidth, bounds.outHeight) / (sample * 2) >= MAX_SIDE) sample *= 2
                val decoded = runCatching {
                    BitmapFactory.decodeFile(raw.path, BitmapFactory.Options().apply { inSampleSize = sample })
                }.getOrNull()
                if (decoded != null) return@run rotate(decoded, rotationOf(raw))
            }
            decodeWithImageDecoder(raw)
        } ?: return null
        val scaled = shrink(upright, MAX_SIDE)
        val out = File(raw.parentFile, raw.nameWithoutExtension + "-s.jpg")
        out.outputStream().use { scaled.compress(Bitmap.CompressFormat.JPEG, 85, it) }
        raw.delete()
        return mapOf(
            "path" to out.path,
            "name" to name.substringBeforeLast('.') + ".jpg",
            "mime" to "image/jpeg",
            "kind" to "image",
            "width" to scaled.width,
            "height" to scaled.height,
            "durationMs" to 0,
            "thumb" to thumb(scaled),
        )
    }

    private fun video(raw: File, name: String, mime: String): Map<String, Any> {
        val retriever = MediaMetadataRetriever()
        return try {
            retriever.setDataSource(raw.path)
            val w = retriever.extractMetadata(MediaMetadataRetriever.METADATA_KEY_VIDEO_WIDTH)?.toIntOrNull() ?: 0
            val h = retriever.extractMetadata(MediaMetadataRetriever.METADATA_KEY_VIDEO_HEIGHT)?.toIntOrNull() ?: 0
            val rotation = retriever.extractMetadata(MediaMetadataRetriever.METADATA_KEY_VIDEO_ROTATION)?.toIntOrNull() ?: 0
            val duration = retriever.extractMetadata(MediaMetadataRetriever.METADATA_KEY_DURATION)?.toIntOrNull() ?: 0
            val frame = retriever.frameAtTime
            val sideways = rotation == 90 || rotation == 270
            mapOf(
                "path" to raw.path,
                "name" to name,
                "mime" to mime,
                "kind" to "video",
                "width" to if (sideways) h else w,
                "height" to if (sideways) w else h,
                "durationMs" to duration,
                "thumb" to (frame?.let { thumb(it) } ?: ByteArray(0)),
            )
        } catch (e: Exception) {
            file(raw, name, mime, "video")
        } finally {
            runCatching { retriever.release() }
        }
    }

    private fun thumb(bitmap: Bitmap): ByteArray {
        val out = ByteArrayOutputStream()
        shrink(bitmap, THUMB_SIDE).compress(Bitmap.CompressFormat.JPEG, 50, out)
        return out.toByteArray()
    }

    private fun shrink(bitmap: Bitmap, side: Int): Bitmap {
        val longest = max(bitmap.width, bitmap.height)
        if (longest <= side) return bitmap
        val scale = side.toFloat() / longest
        return Bitmap.createScaledBitmap(
            bitmap,
            (bitmap.width * scale).roundToInt().coerceAtLeast(1),
            (bitmap.height * scale).roundToInt().coerceAtLeast(1),
            true,
        )
    }

    private fun rotationOf(file: File): Int = runCatching {
        when (ExifInterface(file.path).getAttributeInt(ExifInterface.TAG_ORIENTATION, ExifInterface.ORIENTATION_NORMAL)) {
            ExifInterface.ORIENTATION_ROTATE_90 -> 90
            ExifInterface.ORIENTATION_ROTATE_180 -> 180
            ExifInterface.ORIENTATION_ROTATE_270 -> 270
            else -> 0
        }
    }.getOrDefault(0)

    private fun rotate(bitmap: Bitmap, degrees: Int): Bitmap {
        if (degrees == 0) return bitmap
        val matrix = Matrix().apply { postRotate(degrees.toFloat()) }
        return Bitmap.createBitmap(bitmap, 0, 0, bitmap.width, bitmap.height, matrix, true)
    }

    private fun displayName(resolver: ContentResolver, uri: Uri): String? =
        runCatching {
            resolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use { c ->
                if (c.moveToFirst()) c.getString(0) else null
            }
        }.getOrNull()
}
