package dev.tabscreen

import android.content.Intent
import android.os.Bundle
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

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_main)
        val prefs = getSharedPreferences("tabscreen", MODE_PRIVATE)
        val host = findViewById<EditText>(R.id.host)
        val port = findViewById<EditText>(R.id.port)
        val codec = findViewById<Spinner>(R.id.codec)
        val sendTouch = findViewById<CheckBox>(R.id.sendTouch)
        val mode = findViewById<Spinner>(R.id.mode)
        val audio = findViewById<Spinner>(R.id.audio)
        val status = findViewById<TextView>(R.id.status)

        host.setText(prefs.getString("host", ""))
        port.setText(prefs.getInt("port", 7741).toString())
        sendTouch.isChecked = prefs.getBoolean("sendTouch", true)
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

        findViewById<TextView>(R.id.usbHint).text =
            HtmlCompat.fromHtml(getString(R.string.usb_hint), HtmlCompat.FROM_HTML_MODE_COMPACT)

        findViewById<Button>(R.id.connect).setOnClickListener {
            val h = host.text.toString().trim()
            val p = port.text.toString().toIntOrNull()
            if (h.isEmpty() || p == null || p !in 1..65535) {
                status.text = "Enter a host and a valid port"
                return@setOnClickListener
            }
            val c = options[codec.selectedItemPosition].id
            prefs.edit().putString("host", h).putInt("port", p).putInt("codec", c).putBoolean("sendTouch", sendTouch.isChecked)
                .putInt("mode", mode.selectedItemPosition).putInt("audio", audio.selectedItemPosition).apply()
            startActivity(Intent(this, StreamActivity::class.java).apply {
                putExtra(StreamActivity.EXTRA_HOST, h)
                putExtra(StreamActivity.EXTRA_PORT, p)
                putExtra(StreamActivity.EXTRA_CODEC, c)
                putExtra(StreamActivity.EXTRA_SEND_TOUCH, sendTouch.isChecked)
                putExtra(StreamActivity.EXTRA_MODE, mode.selectedItemPosition)
                putExtra(StreamActivity.EXTRA_AUDIO, audio.selectedItemPosition)
            })
        }
    }
}
