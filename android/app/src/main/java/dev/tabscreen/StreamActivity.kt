package dev.tabscreen

import android.content.pm.ActivityInfo
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.util.Log
import android.view.SurfaceHolder
import android.view.View
import android.view.WindowManager
import android.widget.TextView
import android.widget.Toast
import androidx.appcompat.app.AppCompatActivity
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import java.nio.ByteBuffer

class StreamActivity : AppCompatActivity(), SurfaceHolder.Callback, Connection.Listener, StreamView.InputSink {
    companion object {
        const val EXTRA_HOST = "host"
        const val EXTRA_PORT = "port"
        const val EXTRA_CODEC = "codec"
        const val EXTRA_SEND_TOUCH = "sendTouch"
        const val EXTRA_MODE = "mode"
        const val EXTRA_AUDIO = "audio"
        const val EXTRA_PAIR = "pair"
        private const val TAG = "StreamActivity"
    }

    private lateinit var view: StreamView
    private lateinit var overlay: TextView
    private val ui = Handler(Looper.getMainLooper())
    private var connection: Connection? = null
    private var decoder: VideoDecoder? = null
    private var audio: AudioPlayer? = null
    private var mode = Protocol.MODE_SCREEN
    private var helloSent = false
    private var firstFrame = false
    private var surfaceReady = false
    private var pendingConfig: IntArray? = null
    private var appliedConfig: IntArray? = null

    private val pinger = object : Runnable {
        override fun run() {
            connection?.send(Protocol.ping(StreamView.nowNs()))
            ui.postDelayed(this, 2000)
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_stream)
        view = findViewById(R.id.stream)
        overlay = findViewById(R.id.overlay)
        view.sink = this
        view.sendTouch = intent.getBooleanExtra(EXTRA_SEND_TOUCH, true)
        mode = intent.getIntExtra(EXTRA_MODE, Protocol.MODE_SCREEN)
        if (mode == Protocol.MODE_TOUCHPAD) {
            view.touchpadMode = true
            view.sendTouch = true
            // Keep the panel dark; the hint stays visible as a subtle label.
            overlay.alpha = 0.35f
        }

        // The virtual monitor is sized from this window, so freeze the orientation for the session.
        requestedOrientation = ActivityInfo.SCREEN_ORIENTATION_LOCKED
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        requestHighestRefreshRate()
        WindowCompat.setDecorFitsSystemWindows(window, false)
        WindowInsetsControllerCompat(window, view).apply {
            hide(WindowInsetsCompat.Type.systemBars())
            systemBarsBehavior = WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
        }
        view.holder.addCallback(this)
    }

    /** Pin the panel to its fastest mode at the current resolution (e.g. 120 Hz) instead of adaptive. */
    private fun requestHighestRefreshRate() {
        val d = display ?: return
        val cur = d.mode
        val best = d.supportedModes
            .filter { it.physicalWidth == cur.physicalWidth && it.physicalHeight == cur.physicalHeight }
            .maxByOrNull { it.refreshRate } ?: return
        window.attributes = window.attributes.apply { preferredDisplayModeId = best.modeId }
        Log.i(TAG, "requested display mode ${best.physicalWidth}x${best.physicalHeight}@${best.refreshRate}")
    }

    override fun surfaceCreated(holder: SurfaceHolder) {}

    override fun surfaceChanged(holder: SurfaceHolder, format: Int, width: Int, height: Int) {
        surfaceReady = true
        if (!helloSent) {
            helloSent = true
            connect(width, height)
        } else {
            pendingConfig?.let { applyConfig(it[0], it[1], it[2]) }
        }
    }

    override fun surfaceDestroyed(holder: SurfaceHolder) {
        surfaceReady = false
        decoder?.release()
        decoder = null
        appliedConfig = null
    }

    private fun connect(width: Int, height: Int) {
        val dm = resources.displayMetrics
        val widthMm = width / dm.xdpi * 25.4f
        val heightMm = height / dm.ydpi * 25.4f
        val refresh = (display?.supportedModes?.maxOfOrNull { it.refreshRate } ?: display?.refreshRate ?: 60f).toInt()
        val codecs = VideoDecoder.supportedCodecs()
        val preferred = intent.getIntExtra(EXTRA_CODEC, 0)
        Log.i(TAG, "display ${width}x$height @${refresh}Hz, ${"%.0f".format(widthMm)}x${"%.0f".format(heightMm)}mm, codecs=$codecs preferred=$preferred")
        val audioMode = intent.getIntExtra(EXTRA_AUDIO, Protocol.AUDIO_NONE)
        val hello = Protocol.hello(width, height, refresh, codecs, preferred, widthMm, heightMm, mode, audioMode)
        val pair = intent.getStringExtra(EXTRA_PAIR) ?: ""
        val host = intent.getStringExtra(EXTRA_HOST) ?: "127.0.0.1"
        val port = intent.getIntExtra(EXTRA_PORT, 7741)
        overlay.text = "Connecting to $host:$port…"
        connection = Connection(host, port, pair, hello, this).also { it.start() }
        ui.postDelayed(pinger, 2000)
        if (mode == Protocol.MODE_TOUCHPAD) {
            ui.postDelayed({ overlay.text = "Touchpad\ntap = click · two fingers = scroll / right-click · three/four fingers = KDE gestures" }, 1500)
        }
    }

    // ---- Connection.Listener (reader thread) ----

    override fun onStreamConfig(codec: Int, width: Int, height: Int, fps: Int) {
        Log.i(TAG, "stream: codec=$codec ${width}x$height @$fps")
        pendingConfig = intArrayOf(codec, width, height)
        ui.post { if (surfaceReady) applyConfig(codec, width, height) }
    }

    private fun applyConfig(codec: Int, width: Int, height: Int) {
        // setFixedSize() re-triggers surfaceChanged(); don't tear down a working decoder for it.
        if (decoder != null && appliedConfig?.contentEquals(intArrayOf(codec, width, height)) == true) return
        decoder?.release()
        decoder = null
        view.holder.setFixedSize(width, height)
        try {
            decoder = VideoDecoder(VideoDecoder.mimeFor(codec), width, height, view.holder.surface)
            appliedConfig = intArrayOf(codec, width, height)
            overlay.text = "Waiting for video…"
            connection?.send(Protocol.keyframeRequest())
        } catch (e: Exception) {
            Log.e(TAG, "decoder init failed", e)
            connection?.close("decoder: ${e.message}")
        }
    }

    override fun onVideo(ptsNs: Long, keyframe: Boolean, data: ByteBuffer) {
        val d = decoder
        if (d == null) {
            // Config not applied yet (surface race); the server resends keyframes regularly.
            return
        }
        d.feed(data, ptsNs / 1000, keyframe)
        if (!firstFrame) {
            firstFrame = true
            ui.post { overlay.visibility = View.GONE }
        }
    }

    override fun onAudioConfig(format: Int, rate: Int, channels: Int) {
        Log.i(TAG, "audio: format=$format $rate Hz x$channels")
        ui.post {
            audio?.release()
            audio = try { AudioPlayer(rate, channels) } catch (e: Exception) { Log.e(TAG, "audio init failed", e); null }
        }
    }

    override fun onAudio(ptsNs: Long, data: ByteBuffer) {
        audio?.feed(data)
    }

    override fun onPong(tNs: Long) {
        val rttMs = (StreamView.nowNs() - tNs) / 1e6
        Log.d(TAG, "rtt %.1f ms, frames rendered %d".format(rttMs, decoder?.framesRendered ?: 0))
    }

    override fun onClosed(reason: String) {
        ui.post {
            Toast.makeText(this, "Disconnected: $reason", Toast.LENGTH_LONG).show()
            finish()
        }
    }

    // ---- StreamView.InputSink ----
    override fun send(msg: ByteArray) {
        connection?.send(msg)
    }

    override fun onDestroy() {
        ui.removeCallbacks(pinger)
        connection?.close("activity destroyed")
        connection = null
        decoder?.release()
        decoder = null
        audio?.release()
        audio = null
        super.onDestroy()
    }
}
