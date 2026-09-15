package dev.tabscreen

import android.content.Intent
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.view.View
import android.widget.ArrayAdapter
import android.widget.Button
import android.widget.CheckBox
import android.widget.EditText
import android.widget.Spinner
import android.widget.TextView
import androidx.appcompat.app.AppCompatActivity
import androidx.core.text.HtmlCompat

class MainActivity : AppCompatActivity() {
    private data class CodecOption(val label: String, val id: Int)

    // Connection modes
    private val CONN_WIFI = 0
    private val CONN_USB = 1
    private val CONN_MANUAL = 2

    private val ui = Handler(Looper.getMainLooper())
    private var discovered: List<Discovery.Server> = emptyList()

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_main)
        val prefs = getSharedPreferences("tabscreen", MODE_PRIVATE)

        val connMode = findViewById<Spinner>(R.id.connMode)
        val discoverBox = findViewById<View>(R.id.discoverBox)
        val manualBox = findViewById<View>(R.id.manualBox)
        val discoveredSpinner = findViewById<Spinner>(R.id.discovered)
        val scan = findViewById<Button>(R.id.scan)
        val connHint = findViewById<TextView>(R.id.connHint)
        val host = findViewById<EditText>(R.id.host)
        val port = findViewById<EditText>(R.id.port)
        val codec = findViewById<Spinner>(R.id.codec)
        val sendTouch = findViewById<CheckBox>(R.id.sendTouch)
        val mode = findViewById<Spinner>(R.id.mode)
        val audio = findViewById<Spinner>(R.id.audio)
        val pair = findViewById<EditText>(R.id.pair)
        val status = findViewById<TextView>(R.id.status)

        connMode.adapter = ArrayAdapter(this, android.R.layout.simple_spinner_dropdown_item,
            listOf("Wi-Fi (auto-detect)", "USB", "Manual IP"))
        host.setText(prefs.getString("host", ""))
        port.setText(prefs.getInt("port", 7741).toString())
        sendTouch.isChecked = prefs.getBoolean("sendTouch", true)
        pair.setText(prefs.getString("pair", ""))
        mode.adapter = ArrayAdapter(this, android.R.layout.simple_spinner_dropdown_item, listOf("Second screen (with S Pen)", "Touchpad"))
        mode.setSelection(prefs.getInt("mode", Protocol.MODE_SCREEN))
        audio.adapter = ArrayAdapter(this, android.R.layout.simple_spinner_dropdown_item, listOf("Computer (no audio streaming)", "Tablet (computer goes silent)", "Both"))
        audio.setSelection(prefs.getInt("audio", Protocol.AUDIO_NONE))

        val supported = VideoDecoder.supportedCodecs()
        val options = mutableListOf(CodecOption("Server default", 0))
        if (supported and (1 shl (Protocol.CODEC_H264 - 1)) != 0) options += CodecOption("H.264", Protocol.CODEC_H264)
        if (supported and (1 shl (Protocol.CODEC_HEVC - 1)) != 0) options += CodecOption("HEVC / H.265", Protocol.CODEC_HEVC)
        if (supported and (1 shl (Protocol.CODEC_AV1 - 1)) != 0) options += CodecOption("AV1", Protocol.CODEC_AV1)
        codec.adapter = ArrayAdapter(this, android.R.layout.simple_spinner_dropdown_item, options.map { it.label })
        codec.setSelection(options.indexOfFirst { it.id == prefs.getInt("codec", 0) }.coerceAtLeast(0))

        fun applyConnMode(m: Int) {
            discoverBox.visibility = if (m == CONN_MANUAL) View.GONE else View.VISIBLE
            manualBox.visibility = if (m == CONN_MANUAL) View.VISIBLE else View.GONE
            connHint.text = HtmlCompat.fromHtml(
                getString(when (m) { CONN_USB -> R.string.conn_usb_hint; CONN_MANUAL -> R.string.conn_manual_hint; else -> R.string.conn_wifi_hint }),
                HtmlCompat.FROM_HTML_MODE_COMPACT
            )
        }
        connMode.setSelection(prefs.getInt("connMode", CONN_WIFI))
        applyConnMode(connMode.selectedItemPosition)
        connMode.onItemSelectedListener = object : android.widget.AdapterView.OnItemSelectedListener {
            override fun onItemSelected(p: android.widget.AdapterView<*>?, v: View?, pos: Int, id: Long) = applyConnMode(pos)
            override fun onNothingSelected(p: android.widget.AdapterView<*>?) {}
        }

        fun showDiscovered(list: List<Discovery.Server>) {
            discovered = list
            val labels = if (list.isEmpty()) listOf("No computers found — tap Scan") else list.map { "${it.name}  (${it.host})" }
            discoveredSpinner.adapter = ArrayAdapter(this, android.R.layout.simple_spinner_dropdown_item, labels)
        }
        showDiscovered(emptyList())

        fun doScan() {
            scan.isEnabled = false
            scan.text = "Scanning…"
            // For USB via adb reverse, 127.0.0.1 always works even if broadcast finds nothing.
            Discovery.scan(this) { servers ->
                val list = servers.toMutableList()
                if (connMode.selectedItemPosition == CONN_USB && list.none { it.host == "127.0.0.1" }) {
                    list.add(0, Discovery.Server("127.0.0.1", 7741, "USB (adb reverse)"))
                }
                ui.post {
                    showDiscovered(list)
                    scan.isEnabled = true
                    scan.text = getString(R.string.scan)
                    status.text = if (list.isEmpty()) "Nothing found. Check the server is running and you're on the same network." else ""
                }
            }
        }
        scan.setOnClickListener { doScan() }
        // Auto-scan on launch for the non-manual modes.
        if (connMode.selectedItemPosition != CONN_MANUAL) doScan()

        findViewById<Button>(R.id.connect).setOnClickListener {
            val cm = connMode.selectedItemPosition
            val h: String
            val p: Int
            if (cm == CONN_MANUAL) {
                h = host.text.toString().trim()
                val pp = port.text.toString().toIntOrNull()
                if (h.isEmpty() || pp == null || pp !in 1..65535) { status.text = "Enter a host and a valid port"; return@setOnClickListener }
                p = pp
            } else {
                val sel = discovered.getOrNull(discoveredSpinner.selectedItemPosition)
                if (sel == null) {
                    if (cm == CONN_USB) { h = "127.0.0.1"; p = 7741 }
                    else { status.text = "Tap Scan and pick a computer first"; return@setOnClickListener }
                } else { h = sel.host; p = sel.port }
            }
            val pairCode = pair.text.toString().trim()
            if (pairCode.isEmpty()) { status.text = "Enter the pairing code shown on the computer"; return@setOnClickListener }
            val c = options[codec.selectedItemPosition].id
            prefs.edit().putString("host", h).putInt("port", p).putInt("codec", c).putBoolean("sendTouch", sendTouch.isChecked)
                .putInt("mode", mode.selectedItemPosition).putInt("audio", audio.selectedItemPosition).putInt("connMode", cm)
                .putString("pair", pairCode).apply()
            startActivity(Intent(this, StreamActivity::class.java).apply {
                putExtra(StreamActivity.EXTRA_HOST, h)
                putExtra(StreamActivity.EXTRA_PORT, p)
                putExtra(StreamActivity.EXTRA_CODEC, c)
                putExtra(StreamActivity.EXTRA_SEND_TOUCH, sendTouch.isChecked)
                putExtra(StreamActivity.EXTRA_MODE, mode.selectedItemPosition)
                putExtra(StreamActivity.EXTRA_AUDIO, audio.selectedItemPosition)
                putExtra(StreamActivity.EXTRA_PAIR, pairCode)
            })
        }
    }
}
