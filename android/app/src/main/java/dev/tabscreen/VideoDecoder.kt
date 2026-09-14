package dev.tabscreen

import android.media.MediaCodec
import android.media.MediaCodecInfo
import android.media.MediaCodecList
import android.media.MediaFormat
import android.os.Build
import android.os.Handler
import android.os.HandlerThread
import android.util.Log
import android.view.Surface
import java.nio.ByteBuffer
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.TimeUnit

/**
 * Hardware video decoder tuned for latency: async MediaCodec, low-latency mode
 * where the vendor supports it, and every output frame rendered the moment it
 * is decoded (no A/V sync, no buffering).
 */
class VideoDecoder(private val mime: String, width: Int, height: Int, surface: Surface) {
    private val tag = "VideoDecoder"
    private val thread = HandlerThread("decoder").apply { start() }
    private val codec: MediaCodec
    private val freeInputs = LinkedBlockingQueue<Int>()
    @Volatile private var released = false
    private var awaitingKeyframe = true
    @Volatile var framesRendered = 0L
        private set

    init {
        val format = MediaFormat.createVideoFormat(mime, width, height).apply {
            setInteger(MediaFormat.KEY_MAX_INPUT_SIZE, 4 shl 20)
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
                setInteger(MediaFormat.KEY_LOW_LATENCY, 1)
            }
            setInteger(MediaFormat.KEY_PRIORITY, 0) // realtime
            // Vendor-specific low latency knobs; harmless where unknown.
            setInteger("vendor.qti-ext-dec-low-latency.enable", 1)
            setInteger("vendor.rtc-ext-dec-low-latency.enable", 1)
        }
        val name = pickDecoder(mime, format)
        Log.i(tag, "using decoder $name for $mime ${width}x$height")
        codec = MediaCodec.createByCodecName(name)
        codec.setCallback(object : MediaCodec.Callback() {
            override fun onInputBufferAvailable(c: MediaCodec, index: Int) {
                freeInputs.offer(index)
            }

            override fun onOutputBufferAvailable(c: MediaCodec, index: Int, info: MediaCodec.BufferInfo) {
                if (released) return
                try {
                    c.releaseOutputBuffer(index, true)
                    framesRendered++
                } catch (e: IllegalStateException) {
                    Log.w(tag, "release output failed", e)
                }
            }

            override fun onError(c: MediaCodec, e: MediaCodec.CodecException) {
                Log.e(tag, "codec error: ${e.diagnosticInfo}", e)
            }

            override fun onOutputFormatChanged(c: MediaCodec, f: MediaFormat) {
                Log.i(tag, "output format: $f")
            }
        }, Handler(thread.looper))
        codec.configure(format, surface, null, 0)
        codec.start()
    }

    private fun pickDecoder(mime: String, format: MediaFormat): String {
        val list = MediaCodecList(MediaCodecList.REGULAR_CODECS)
        val candidates = list.codecInfos.filter { !it.isEncoder && it.supportedTypes.any { t -> t.equals(mime, true) } }
        // Prefer hardware decoders that accept the exact format.
        val hw = candidates.filter { it.isHardwareAcceleratedCompat() }
        val exact = format.let { f ->
            val probe = MediaFormat().apply {
                setString(MediaFormat.KEY_MIME, mime)
                setInteger(MediaFormat.KEY_WIDTH, f.getInteger(MediaFormat.KEY_WIDTH))
                setInteger(MediaFormat.KEY_HEIGHT, f.getInteger(MediaFormat.KEY_HEIGHT))
            }
            (hw + candidates).firstOrNull { it.getCapabilitiesForType(mime).isFormatSupported(probe) }
        }
        return (exact ?: hw.firstOrNull() ?: candidates.firstOrNull())?.name
            ?: throw IllegalStateException("no decoder for $mime")
    }

    private fun MediaCodecInfo.isHardwareAcceleratedCompat(): Boolean =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) isHardwareAccelerated
        else !name.startsWith("OMX.google.") && !name.startsWith("c2.android.")

    /** Feed one complete access unit (Annex-B / OBU stream). Blocks briefly if the decoder is saturated. */
    fun feed(data: ByteBuffer, ptsUs: Long, keyframe: Boolean) {
        if (released) return
        // A fresh decoder must start on an IDR (with in-band SPS/PPS); skip deltas until then.
        if (awaitingKeyframe) {
            if (!keyframe) return
            awaitingKeyframe = false
        }
        val index = freeInputs.poll(200, TimeUnit.MILLISECONDS) ?: run {
            Log.w(tag, "decoder stalled, dropping frame")
            return
        }
        try {
            val buf = codec.getInputBuffer(index) ?: return
            buf.clear()
            val n = minOf(data.remaining(), buf.remaining())
            val slice = data.duplicate().apply { limit(position() + n) }
            buf.put(slice)
            val flags = if (keyframe) MediaCodec.BUFFER_FLAG_KEY_FRAME else 0
            codec.queueInputBuffer(index, 0, n, ptsUs, flags)
        } catch (e: IllegalStateException) {
            Log.w(tag, "queue input failed", e)
        }
    }

    fun release() {
        released = true
        try { codec.stop() } catch (_: Exception) {}
        try { codec.release() } catch (_: Exception) {}
        thread.quitSafely()
    }

    companion object {
        fun mimeFor(codec: Int): String = when (codec) {
            Protocol.CODEC_H264 -> MediaFormat.MIMETYPE_VIDEO_AVC
            Protocol.CODEC_HEVC -> MediaFormat.MIMETYPE_VIDEO_HEVC
            Protocol.CODEC_AV1 -> MediaFormat.MIMETYPE_VIDEO_AV1
            else -> throw IllegalArgumentException("unknown codec $codec")
        }

        /** Bitmask of codecs with a hardware decoder on this device (bit = codec id - 1). */
        fun supportedCodecs(): Int {
            val list = MediaCodecList(MediaCodecList.REGULAR_CODECS)
            var mask = 0
            for (c in intArrayOf(Protocol.CODEC_H264, Protocol.CODEC_HEVC, Protocol.CODEC_AV1)) {
                val mime = mimeFor(c)
                val ok = list.codecInfos.any { info ->
                    !info.isEncoder && info.supportedTypes.any { it.equals(mime, true) } &&
                        (Build.VERSION.SDK_INT < Build.VERSION_CODES.Q || info.isHardwareAccelerated)
                }
                if (ok) mask = mask or (1 shl (c - 1))
            }
            return mask
        }
    }
}
