package dev.tabscreen

import android.media.AudioAttributes
import android.media.AudioFormat
import android.media.AudioManager
import android.media.AudioTrack
import android.util.Log
import java.nio.ByteBuffer
import java.util.concurrent.LinkedBlockingDeque

/**
 * Plays raw PCM chunks from the server with a bounded queue: if the network
 * hiccups we drop old audio rather than let latency accumulate.
 */
class AudioPlayer(rate: Int, channels: Int) {
    private val tag = "AudioPlayer"
    private val bytesPerSecond = rate * channels * 2
    private val maxQueuedBytes = bytesPerSecond / 10 // ~100 ms
    private val queue = LinkedBlockingDeque<ByteArray>()
    @Volatile private var queuedBytes = 0
    @Volatile private var running = true
    private val track: AudioTrack
    private val thread: Thread

    init {
        val channelMask = if (channels == 1) AudioFormat.CHANNEL_OUT_MONO else AudioFormat.CHANNEL_OUT_STEREO
        val minBuf = AudioTrack.getMinBufferSize(rate, channelMask, AudioFormat.ENCODING_PCM_16BIT)
        track = AudioTrack.Builder()
            .setAudioAttributes(
                AudioAttributes.Builder()
                    .setUsage(AudioAttributes.USAGE_MEDIA)
                    .setContentType(AudioAttributes.CONTENT_TYPE_MUSIC)
                    .build()
            )
            .setAudioFormat(
                AudioFormat.Builder()
                    .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                    .setSampleRate(rate)
                    .setChannelMask(channelMask)
                    .build()
            )
            .setBufferSizeInBytes(maxOf(minBuf, bytesPerSecond / 20))
            .setPerformanceMode(AudioTrack.PERFORMANCE_MODE_LOW_LATENCY)
            .setTransferMode(AudioTrack.MODE_STREAM)
            .build()
        track.play()
        Log.i(tag, "audio ${rate} Hz x$channels, buffer ${track.bufferSizeInFrames} frames")
        thread = Thread({
            while (running) {
                val chunk = try { queue.takeFirst() } catch (_: InterruptedException) { break }
                queuedBytes -= chunk.size
                var off = 0
                while (off < chunk.size && running) {
                    val n = track.write(chunk, off, chunk.size - off, AudioTrack.WRITE_BLOCKING)
                    if (n < 0) { Log.w(tag, "write error $n"); break }
                    off += n
                }
            }
        }, "audio-out").apply { start() }
    }

    fun feed(data: ByteBuffer) {
        val bytes = ByteArray(data.remaining()).also { data.get(it) }
        while (queuedBytes + bytes.size > maxQueuedBytes) {
            val dropped = queue.pollFirst() ?: break
            queuedBytes -= dropped.size
        }
        queuedBytes += bytes.size
        queue.offerLast(bytes)
    }

    fun release() {
        running = false
        thread.interrupt()
        try { track.pause(); track.flush(); track.release() } catch (_: Exception) {}
    }

    companion object {
        fun isStreamMuted(am: AudioManager) = am.getStreamVolume(AudioManager.STREAM_MUSIC) == 0
    }
}
