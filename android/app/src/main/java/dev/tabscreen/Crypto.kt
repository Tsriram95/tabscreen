package dev.tabscreen

import javax.crypto.Mac
import javax.crypto.spec.SecretKeySpec

/** RFC 4648 base32 (no padding) + HMAC-SHA256 helpers for pairing. */
object Crypto {
    private const val ALPHABET = "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567"

    /** Decode a pairing code (dashes/spaces/case ignored) to the raw token bytes. */
    fun decodePairingCode(code: String): ByteArray {
        val s = code.trim().uppercase().replace("-", "").replace(" ", "")
        val out = ArrayList<Byte>(s.length * 5 / 8)
        var buffer = 0
        var bits = 0
        for (c in s) {
            val v = ALPHABET.indexOf(c)
            require(v >= 0) { "invalid character in pairing code: $c" }
            buffer = (buffer shl 5) or v
            bits += 5
            if (bits >= 8) {
                bits -= 8
                out.add(((buffer ushr bits) and 0xFF).toByte())
            }
        }
        return out.toByteArray()
    }

    fun hmacSha256(key: ByteArray, vararg parts: ByteArray): ByteArray {
        val mac = Mac.getInstance("HmacSHA256")
        mac.init(SecretKeySpec(key, "HmacSHA256"))
        for (p in parts) mac.update(p)
        return mac.doFinal()
    }

    fun constantTimeEquals(a: ByteArray, b: ByteArray): Boolean {
        if (a.size != b.size) return false
        var diff = 0
        for (i in a.indices) diff = diff or (a[i].toInt() xor b[i].toInt())
        return diff == 0
    }
}
