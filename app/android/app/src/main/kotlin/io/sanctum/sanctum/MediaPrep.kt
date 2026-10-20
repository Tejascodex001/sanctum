package io.sanctum.sanctum

import android.content.ContentResolver
import android.content.Context
import android.graphics.Bitmap
import android.graphics.BitmapFactory
import android.graphics.Matrix
import android.media.ExifInterface
import android.media.MediaMetadataRetriever
import android.net.Uri
import android.provider.OpenableColumns
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

    fun prepare(context: Context, uri: Uri): Map<String, Any> {
        val resolver = context.contentResolver
        val mime = resolver.getType(uri) ?: "application/octet-stream"
        val name = displayName(resolver, uri) ?: "file"
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

    private fun image(raw: File, name: String): Map<String, Any>? {
        val bounds = BitmapFactory.Options().apply { inJustDecodeBounds = true }
        BitmapFactory.decodeFile(raw.path, bounds)
        if (bounds.outWidth <= 0 || bounds.outHeight <= 0) return null
        var sample = 1
        while (max(bounds.outWidth, bounds.outHeight) / (sample * 2) >= MAX_SIDE) sample *= 2
        val decoded = BitmapFactory.decodeFile(raw.path, BitmapFactory.Options().apply { inSampleSize = sample }) ?: return null
        val upright = rotate(decoded, rotationOf(raw))
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
