package dev.tabscreen

import android.content.Context
import android.net.wifi.WifiManager
import android.util.Log
import java.net.DatagramPacket
import java.net.DatagramSocket
import java.net.InetAddress
import java.net.InetSocketAddress
import java.nio.ByteBuffer
import java.nio.ByteOrder
import kotlin.concurrent.thread

/**
 * Finds TabScreen servers by UDP broadcast — no mDNS/Avahi needed. Works over
 * Wi-Fi and over a USB-tethering link (same broadcast domain). The server
 * answers on UDP :7742 with its TCP port and hostname.
 */
object Discovery {
    private const val TAG = "Discovery"
    const val PORT = 7742
    private val REQUEST = "TABSCREEN?".toByteArray()
    private val RESPONSE_MAGIC = "TABSCREEN!".toByteArray()

    data class Server(val host: String, val port: Int, val name: String)

    /** Broadcasts for [timeoutMs] and calls [onResult] on the main-less caller thread with the deduped list. */
    fun scan(context: Context, timeoutMs: Int = 1500, onResult: (List<Server>) -> Unit) {
        thread(name = "discovery-scan") {
            val found = LinkedHashMap<String, Server>()
            var lock: WifiManager.MulticastLock? = null
            try {
                // Broadcast reception on some ROMs needs a multicast lock held.
                (context.applicationContext.getSystemService(Context.WIFI_SERVICE) as? WifiManager)?.let {
                    lock = it.createMulticastLock("tabscreen-scan").apply { setReferenceCounted(false); acquire() }
                }
                DatagramSocket().use { sock ->
                    sock.broadcast = true
                    sock.soTimeout = 200
                    val targets = broadcastAddresses(context)
                    for (addr in targets) {
                        try {
                            sock.send(DatagramPacket(REQUEST, REQUEST.size, InetSocketAddress(addr, PORT)))
                        } catch (e: Exception) {
                            Log.d(TAG, "send to $addr failed: ${e.message}")
                        }
                    }
                    val deadline = System.currentTimeMillis() + timeoutMs
                    val buf = ByteArray(256)
                    while (System.currentTimeMillis() < deadline) {
                        val pkt = DatagramPacket(buf, buf.size)
                        try {
                            sock.receive(pkt)
                        } catch (_: Exception) {
                            continue // timeout tick
                        }
                        parse(pkt)?.let { found[it.host] = it }
                    }
                }
            } catch (e: Exception) {
                Log.w(TAG, "scan failed", e)
            } finally {
                try { lock?.release() } catch (_: Exception) {}
            }
            onResult(found.values.toList())
        }
    }

    private fun parse(pkt: DatagramPacket): Server? {
        val data = pkt.data.copyOf(pkt.length)
        if (data.size < RESPONSE_MAGIC.size + 2) return null
        if (!data.copyOf(RESPONSE_MAGIC.size).contentEquals(RESPONSE_MAGIC)) return null
        val bb = ByteBuffer.wrap(data, RESPONSE_MAGIC.size, data.size - RESPONSE_MAGIC.size).order(ByteOrder.LITTLE_ENDIAN)
        val port = bb.short.toInt() and 0xffff
        val nameBytes = ByteArray(bb.remaining()).also { bb.get(it) }
        val name = String(nameBytes).ifBlank { "computer" }
        return Server(pkt.address.hostAddress ?: return null, port, name)
    }

    /** 255.255.255.255 plus the directed broadcast of each non-loopback IPv4 interface. */
    private fun broadcastAddresses(context: Context): List<InetAddress> {
        val out = linkedSetOf<InetAddress>()
        try { out.add(InetAddress.getByName("255.255.255.255")) } catch (_: Exception) {}
        try {
            java.net.NetworkInterface.getNetworkInterfaces()?.toList()?.forEach { nif ->
                if (!nif.isUp || nif.isLoopback) return@forEach
                nif.interfaceAddresses.forEach { ia ->
                    ia.broadcast?.let { out.add(it) }
                }
            }
        } catch (_: Exception) {}
        return out.toList()
    }
}
