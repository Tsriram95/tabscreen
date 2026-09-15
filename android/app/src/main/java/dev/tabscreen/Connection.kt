package dev.tabscreen

import android.util.Log
import java.io.BufferedInputStream
import java.io.DataInputStream
import java.net.InetSocketAddress
import java.net.Socket
import java.security.MessageDigest
import java.security.SecureRandom
import java.security.cert.X509Certificate
import java.util.concurrent.LinkedBlockingQueue
import java.util.concurrent.atomic.AtomicBoolean
import javax.net.ssl.SSLContext
import javax.net.ssl.SSLSocket
import javax.net.ssl.X509TrustManager

/**
 * TLS session with the server plus the pairing handshake. The connection is
 * encrypted; the server's self-signed certificate is accepted, but the pairing
 * code (a shared secret) is HMAC-bound to that certificate's fingerprint, so a
 * man-in-the-middle presenting a different certificate cannot authenticate.
 */
class Connection(
    private val host: String,
    private val port: Int,
    private val pairingCode: String,
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
    private var socket: SSLSocket? = null
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
            outQueue.offer(ByteArray(0))
            listener.onClosed(reason)
        }
    }

    private fun trustAllContext(): SSLContext {
        // Accept any server cert; authenticity is enforced by the pairing HMAC
        // over the cert fingerprint, not by a CA.
        val tm = object : X509TrustManager {
            override fun checkClientTrusted(c: Array<out X509Certificate>?, a: String?) {}
            override fun checkServerTrusted(c: Array<out X509Certificate>?, a: String?) {}
            override fun getAcceptedIssuers(): Array<X509Certificate> = arrayOf()
        }
        return SSLContext.getInstance("TLS").apply { init(null, arrayOf(tm), SecureRandom()) }
    }

    private fun runReader() {
        try {
            val plain = Socket()
            plain.tcpNoDelay = true
            plain.receiveBufferSize = 4 shl 20
            plain.connect(InetSocketAddress(host, port), 5000)
            val ssl = (trustAllContext().socketFactory.createSocket(plain, host, port, true) as SSLSocket)
            ssl.useClientMode = true
            ssl.startHandshake()
            socket = ssl

            // Pairing handshake before anything else.
            val din = DataInputStream(BufferedInputStream(ssl.inputStream, 1 shl 20))
            val out = ssl.outputStream
            val token = Crypto.decodePairingCode(pairingCode)
            val certFp = MessageDigest.getInstance("SHA-256")
                .digest((ssl.session.peerCertificates[0] as X509Certificate).encoded)

            val challenge = Protocol.readFrame(din)
            if (challenge.type != Protocol.MSG_AUTH_CHALLENGE) throw Exception("no auth challenge")
            val nonceS = ByteArray(challenge.payload.remaining()).also { challenge.payload.get(it) }
            val nonceC = ByteArray(16).also { SecureRandom().nextBytes(it) }
            val macC = Crypto.hmacSha256(token, "tabscreen-client".toByteArray(), nonceS, nonceC, certFp)
            out.write(byteArrayOf(Protocol.MSG_AUTH_RESPONSE.toByte()) + intLe(nonceC.size + macC.size) + nonceC + macC)
            out.flush()

            val reply = Protocol.readFrame(din)
            if (reply.type == Protocol.MSG_AUTH_FAIL) {
                val why = ByteArray(reply.payload.remaining()).also { reply.payload.get(it) }.toString(Charsets.UTF_8)
                throw Exception("pairing rejected: $why")
            }
            if (reply.type != Protocol.MSG_AUTH_OK) throw Exception("unexpected auth reply ${reply.type}")
            val macS = ByteArray(reply.payload.remaining()).also { reply.payload.get(it) }
            val expectS = Crypto.hmacSha256(token, "tabscreen-server".toByteArray(), nonceS, nonceC, certFp)
            if (!Crypto.constantTimeEquals(macS, expectS)) throw Exception("server failed pairing (wrong code?)")

            // Authenticated: send HELLO and start streaming.
            out.write(hello); out.flush()
            writer = Thread({ runWriter() }, "net-tx").apply { start() }

            while (!closed.get()) {
                val f = Protocol.readFrame(din)
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
            try { socket?.close() } catch (_: Exception) {}
        }
    }

    private fun runWriter() {
        try {
            val out = socket!!.outputStream
            while (!closed.get()) {
                val first = outQueue.take()
                if (first.isEmpty()) break
                out.write(first)
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

    private fun intLe(v: Int) = byteArrayOf(
        (v and 0xff).toByte(), ((v ushr 8) and 0xff).toByte(),
        ((v ushr 16) and 0xff).toByte(), ((v ushr 24) and 0xff).toByte()
    )
}
