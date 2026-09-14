package dev.tabscreen

import java.io.DataInputStream
import java.io.EOFException
import java.io.OutputStream
import java.nio.ByteBuffer
import java.nio.ByteOrder

/** Wire protocol shared with the Rust server (see PROTOCOL.md). Little-endian throughout. */
object Protocol {
    const val VERSION = 2

    const val MSG_HELLO = 0x01
    const val MSG_PEN = 0x02
    const val MSG_TOUCH = 0x03
    const val MSG_PING = 0x04
    const val MSG_KEYFRAME_REQUEST = 0x05

    const val MSG_STREAM_CONFIG = 0x81
    const val MSG_VIDEO = 0x82
    const val MSG_AUDIO_CONFIG = 0x83
    const val MSG_PONG = 0x84
    const val MSG_AUDIO = 0x85

    const val MODE_SCREEN = 0
    const val MODE_TOUCHPAD = 1

    const val AUDIO_NONE = 0
    const val AUDIO_TABLET = 1
    const val AUDIO_BOTH = 2

    const val CODEC_H264 = 1
    const val CODEC_HEVC = 2
    const val CODEC_AV1 = 3

    const val VIDEO_FLAG_KEYFRAME = 1

    const val PEN_HOVER_ENTER = 0
    const val PEN_HOVER_MOVE = 1
    const val PEN_HOVER_EXIT = 2
    const val PEN_DOWN = 3
    const val PEN_MOVE = 4
    const val PEN_UP = 5
    const val PEN_CANCEL = 6

    const val TOOL_PEN = 1
    const val TOOL_ERASER = 2

    const val BTN_PRIMARY = 1
    const val BTN_SECONDARY = 2

    const val TOUCH_DOWN = 0
    const val TOUCH_MOVE = 1
    const val TOUCH_UP = 2
    const val TOUCH_CANCEL = 3

    const val PEN_EVENT_SIZE = 36
    const val TOUCH_EVENT_SIZE = 28

    class Frame(val type: Int, val payload: ByteBuffer)

    fun readFrame(input: DataInputStream): Frame {
        val hdr = ByteArray(5)
        input.readFully(hdr)
        val len = ByteBuffer.wrap(hdr, 1, 4).order(ByteOrder.LITTLE_ENDIAN).int
        if (len < 0 || len > 64 shl 20) throw EOFException("bad frame length $len")
        val payload = ByteArray(len)
        input.readFully(payload)
        return Frame(hdr[0].toInt() and 0xff, ByteBuffer.wrap(payload).order(ByteOrder.LITTLE_ENDIAN))
    }

    private fun frame(type: Int, payloadLen: Int): ByteBuffer {
        val b = ByteBuffer.allocate(5 + payloadLen).order(ByteOrder.LITTLE_ENDIAN)
        b.put(type.toByte())
        b.putInt(payloadLen)
        return b
    }

    fun hello(width: Int, height: Int, refresh: Int, codecs: Int, preferred: Int, widthMm: Float, heightMm: Float, mode: Int, audio: Int): ByteArray {
        val b = frame(MSG_HELLO, 2 + 2 + 2 + 2 + 1 + 1 + 4 + 4 + 1 + 1)
        b.putShort(VERSION.toShort())
        b.putShort(width.toShort())
        b.putShort(height.toShort())
        b.putShort(refresh.toShort())
        b.put(codecs.toByte())
        b.put(preferred.toByte())
        b.putFloat(widthMm)
        b.putFloat(heightMm)
        b.put(mode.toByte())
        b.put(audio.toByte())
        return b.array()
    }

    fun keyframeRequest(): ByteArray = frame(MSG_KEYFRAME_REQUEST, 0).array()

    fun ping(tNs: Long): ByteArray {
        val b = frame(MSG_PING, 8)
        b.putLong(tNs)
        return b.array()
    }

    /** Builds a MSG_PEN batch. [count] events must already have been written into [events]. */
    class PenBatch(capacity: Int) {
        private val buf = ByteBuffer.allocate(5 + 2 + capacity * PEN_EVENT_SIZE).order(ByteOrder.LITTLE_ENDIAN)
        private var count = 0

        init { reset() }

        fun reset() {
            buf.clear()
            buf.put(MSG_PEN.toByte()); buf.putInt(0); buf.putShort(0)
            count = 0
        }

        fun add(tNs: Long, action: Int, tool: Int, buttons: Int, x: Float, y: Float, pressure: Float, tiltX: Float, tiltY: Float, distance: Float) {
            if (buf.remaining() < PEN_EVENT_SIZE) return
            buf.putLong(tNs); buf.put(action.toByte()); buf.put(tool.toByte()); buf.put(buttons.toByte()); buf.put(0)
            buf.putFloat(x); buf.putFloat(y); buf.putFloat(pressure); buf.putFloat(tiltX); buf.putFloat(tiltY); buf.putFloat(distance)
            count++
        }

        fun build(): ByteArray? {
            if (count == 0) return null
            buf.putInt(1, buf.position() - 5)
            buf.putShort(5, count.toShort())
            return buf.array().copyOf(buf.position())
        }
    }

    class TouchBatch(capacity: Int) {
        private val buf = ByteBuffer.allocate(5 + 2 + capacity * TOUCH_EVENT_SIZE).order(ByteOrder.LITTLE_ENDIAN)
        private var count = 0

        init { reset() }

        fun reset() {
            buf.clear()
            buf.put(MSG_TOUCH.toByte()); buf.putInt(0); buf.putShort(0)
            count = 0
        }

        fun add(tNs: Long, action: Int, id: Int, x: Float, y: Float, pressure: Float, major: Float) {
            if (buf.remaining() < TOUCH_EVENT_SIZE) return
            buf.putLong(tNs); buf.put(action.toByte()); buf.put(id.toByte()); buf.putShort(0)
            buf.putFloat(x); buf.putFloat(y); buf.putFloat(pressure); buf.putFloat(major)
            count++
        }

        fun build(): ByteArray? {
            if (count == 0) return null
            buf.putInt(1, buf.position() - 5)
            buf.putShort(5, count.toShort())
            return buf.array().copyOf(buf.position())
        }
    }

    fun write(out: OutputStream, msg: ByteArray) {
        out.write(msg)
    }
}
