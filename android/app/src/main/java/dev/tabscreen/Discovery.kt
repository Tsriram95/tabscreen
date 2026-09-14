package dev.tabscreen

import android.content.Context
import android.net.wifi.WifiManager
import android.util.Log
import java.io.DataInputStream
import java.net.DatagramPacket
import java.net.DatagramSocket
import java.net.InetAddress
import java.net.InetSocketAddress
import java.net.Socket
import java.nio.ByteBuffer
import java.nio.ByteOrder
import java.util.Collections
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import kotlin.concurrent.thread

/**
 * Finds TabScreen servers two ways, in parallel:
 *  1. UDP broadcast on :7742 — fast, but blocked by many firewalls.
 *  2. TCP probe of every host in the local /24 on the stream port (7741) —
 *     works whenever an actual connection would, i.e. regardless of the
 *     firewall's UDP rules.
 */
object Discovery {
    private const val TAG = "Discovery"
    const val UDP_PORT = 7742
    const val TCP_PORT = 7741
    private val REQUEST = "TABSCREEN?".toByteArray()
    private val RESPONSE_MAGIC = "TABSCREEN!".toByteArray()

    private const val MSG_DISCOVER = 0x06

    data class Server(val host: String, val port: Int, val name: String)

    fun scan(context: Context, timeoutMs: Int = 1800, onResult: (List<Server>) -> Unit) {
        thread(name = "discovery-scan") {
            val found = Collections.synchronizedMap(LinkedHashMap<String, Server>())
            var lock: WifiManager.MulticastLock? = null
            try {
                (context.applicationContext.getSystemService(Context.WIFI_SERVICE) as? WifiManager)?.let {
                    lock = it.createMulticastLock("tabscreen-scan").apply { setReferenceCounted(false); acquire() }
                }
                val udp = thread { udpScan(context, timeoutMs, found) }
                val tcp = thread { tcpScan(context, found) }
                udp.join((timeoutMs + 500).toLong())
                tcp.join((timeoutMs + 1500).toLong())
            } catch (e: Exception) {
                Log.w(TAG, "scan failed", e)
            } finally {
                try { lock?.release() } catch (_: Exception) {}
            }
            onResult(found.values.toList())
        }
    }

    private fun udpScan(context: Context, timeoutMs: Int, found: MutableMap<String, Server>) {
        try {
            DatagramSocket().use { sock ->
                sock.broadcast = true
                sock.soTimeout = 200
                for (addr in broadcastAddresses(context)) {
                    try { sock.send(DatagramPacket(REQUEST, REQUEST.size, InetSocketAddress(addr, UDP_PORT))) }
                    catch (e: Exception) { Log.d(TAG, "udp send $addr: ${e.message}") }
                }
                val deadline = System.currentTimeMillis() + timeoutMs
                val buf = ByteArray(256)
                while (System.currentTimeMillis() < deadline) {
                    val pkt = DatagramPacket(buf, buf.size)
                    try { sock.receive(pkt) } catch (_: Exception) { continue }
                    parseUdp(pkt)?.let { found[it.host] = it }
                }
            }
        } catch (e: Exception) { Log.d(TAG, "udp scan: ${e.message}") }
    }

    private fun parseUdp(pkt: DatagramPacket): Server? {
        val data = pkt.data.copyOf(pkt.length)
        if (data.size < RESPONSE_MAGIC.size + 2) return null
        if (!data.copyOf(RESPONSE_MAGIC.size).contentEquals(RESPONSE_MAGIC)) return null
        val bb = ByteBuffer.wrap(data, RESPONSE_MAGIC.size, data.size - RESPONSE_MAGIC.size).order(ByteOrder.LITTLE_ENDIAN)
        val port = bb.short.toInt() and 0xffff
        val name = ByteArray(bb.remaining()).also { bb.get(it) }.toString(Charsets.UTF_8).ifBlank { "computer" }
        return Server(pkt.address.hostAddress ?: return null, port, name)
    }

    /** Probe every /24 host on TCP 7741 with a MSG_DISCOVER handshake. */
    private fun tcpScan(context: Context, found: MutableMap<String, Server>) {
        val bases = localSubnetPrefixes(context)
        if (bases.isEmpty()) return
        val pool = Executors.newFixedThreadPool(48)
        try {
            for (base in bases) for (i in 1..254) {
                val host = "$base$i"
                pool.execute {
                    if (found.containsKey(host)) return@execute
                    probeTcp(host)?.let { found[host] = it }
                }
            }
        } finally {
            pool.shutdown()
            pool.awaitTermination(4, TimeUnit.SECONDS)
        }
    }

    private fun probeTcp(host: String): Server? {
        return try {
            Socket().use { s ->
                s.tcpNoDelay = true
                s.connect(InetSocketAddress(host, TCP_PORT), 250)
                s.soTimeout = 400
                // frame: u8 type, u32 len(=0)
                s.getOutputStream().apply { write(byteArrayOf(MSG_DISCOVER.toByte(), 0, 0, 0, 0)); flush() }
                val din = DataInputStream(s.getInputStream())
                val hdr = ByteArray(5); din.readFully(hdr)
                if ((hdr[0].toInt() and 0xff) != MSG_DISCOVER) return null
                val len = ByteBuffer.wrap(hdr, 1, 4).order(ByteOrder.LITTLE_ENDIAN).int
                if (len < 0 || len > 256) return null
                val name = ByteArray(len).also { din.readFully(it) }.toString(Charsets.UTF_8).ifBlank { "computer" }
                Server(host, TCP_PORT, name)
            }
        } catch (_: Exception) { null }
    }

    private fun broadcastAddresses(context: Context): List<InetAddress> {
        val out = linkedSetOf<InetAddress>()
        try { out.add(InetAddress.getByName("255.255.255.255")) } catch (_: Exception) {}
        try {
            java.net.NetworkInterface.getNetworkInterfaces()?.toList()?.forEach { nif ->
                if (!nif.isUp || nif.isLoopback) return@forEach
                nif.interfaceAddresses.forEach { ia -> ia.broadcast?.let { out.add(it) } }
            }
        } catch (_: Exception) {}
        return out.toList()
    }

    /** "10.0.0." style prefixes for each up IPv4 /24 the device is on. */
    private fun localSubnetPrefixes(context: Context): List<String> {
        val out = linkedSetOf<String>()
        try {
            java.net.NetworkInterface.getNetworkInterfaces()?.toList()?.forEach { nif ->
                if (!nif.isUp || nif.isLoopback) return@forEach
                nif.interfaceAddresses.forEach { ia ->
                    val a = ia.address
                    if (a is java.net.Inet4Address && ia.networkPrefixLength >= 24) {
                        val ip = a.hostAddress ?: return@forEach
                        out.add(ip.substringBeforeLast('.') + ".")
                    }
                }
            }
        } catch (_: Exception) {}
        return out.toList()
    }
}
