package dev.tabscreen

import android.util.Log
import java.io.BufferedInputStream
import java.io.DataInputStream
import java.net.InetSocketAddress
import java.net.Socket
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.atomic.AtomicBoolean

/**
 * TCP session with the server: one thread reading video frames, one thread
 * writing input events. Callbacks run on the reader thread.
 */
class Connection(
    private val host: String,
    private val port: Int,
    private val hello: ByteArray,
    private val listener: Listener,
) {
    interface Listener {
        fun onStreamConfig(codec: Int, width: Int, height: Int, fps: Int)
        fun onVideo(ptsNs: Long, keyframe: Boolean, data: java.nio.ByteBuffer)
        fun onAudioConfig(format: Int, rate: Int, channels: Int)
        fun onAudio(ptsNs: Long, data: java.nio.ByteBuffer)
        fun onPong(tNs: Long)
        fun onClosed(reason: String)
    }

    private val tag = "Connection"
    private val outQueue = LinkedBlockingQueue<ByteArray>()
    private val closed = AtomicBoolean(false)
    private var socket: Socket? = null
    private var reader: Thread? = null
    private var writer: Thread? = null

    fun start() {
        reader = Thread({ runReader() }, "net-rx").apply { start() }
    }

    fun send(msg: ByteArray) {
        if (!closed.get()) outQueue.offer(msg)
    }

    fun close(reason: String = "closed") {
        if (closed.compareAndSet(false, true)) {
            try { socket?.close() } catch (_: Exception) {}
            outQueue.offer(ByteArray(0)) // wake writer
            listener.onClosed(reason)
        }
    }

    private fun runReader() {
        val sock = Socket()
        try {
            sock.tcpNoDelay = true
            sock.receiveBufferSize = 4 shl 20
            sock.connect(InetSocketAddress(host, port), 5000)
            socket = sock
            val out = sock.getOutputStream()
            out.write(hello)
            out.flush()
            writer = Thread({ runWriter() }, "net-tx").apply { start() }

            val input = DataInputStream(BufferedInputStream(sock.getInputStream(), 1 shl 20))
            while (!closed.get()) {
                val f = Protocol.readFrame(input)
                val p = f.payload
                when (f.type) {
                    Protocol.MSG_STREAM_CONFIG -> {
                        val codec = p.get().toInt() and 0xff
                        val w = p.short.toInt() and 0xffff
                        val h = p.short.toInt() and 0xffff
                        val fps = p.short.toInt() and 0xffff
                        listener.onStreamConfig(codec, w, h, fps)
                    }
                    Protocol.MSG_VIDEO -> {
                        val pts = p.long
                        val flags = p.get().toInt() and 0xff
                        listener.onVideo(pts, flags and Protocol.VIDEO_FLAG_KEYFRAME != 0, p.slice())
                    }
                    Protocol.MSG_AUDIO_CONFIG -> {
                        val format = p.get().toInt() and 0xff
                        val rate = p.int
                        val channels = p.get().toInt() and 0xff
                        listener.onAudioConfig(format, rate, channels)
                    }
                    Protocol.MSG_AUDIO -> {
                        val pts = p.long
                        listener.onAudio(pts, p.slice())
                    }
                    Protocol.MSG_PONG -> listener.onPong(p.long)
                    else -> Log.w(tag, "unknown message type ${f.type}")
                }
            }
        } catch (e: Exception) {
            if (!closed.get()) {
                Log.w(tag, "connection ended", e)
                close(e.message ?: e.javaClass.simpleName)
            }
        } finally {
            try { sock.close() } catch (_: Exception) {}
        }
    }

    private fun runWriter() {
        try {
            val out = socket!!.getOutputStream()
            while (!closed.get()) {
                val first = outQueue.take()
                if (first.isEmpty()) break
                out.write(first)
                // Coalesce whatever else is queued into the same write.
                while (true) {
                    val next = outQueue.poll() ?: break
                    if (next.isEmpty()) break
                    out.write(next)
                }
                out.flush()
            }
        } catch (e: Exception) {
            if (!closed.get()) close(e.message ?: "write failed")
        }
    }
}
