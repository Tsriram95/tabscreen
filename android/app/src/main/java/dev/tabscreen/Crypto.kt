package dev.tabscreen

import javax.crypto.Mac
import javax.crypto.spec.SecretKeySpec

/** Pairing helpers: HMAC-SHA256 over the pairing-code key. */
object Crypto {
    /** The pairing code (case/dash/space-insensitive) as raw HMAC-key bytes. */
    fun decodePairingCode(code: String): ByteArray =
        code.trim().uppercase().replace("-", "").replace(" ", "").toByteArray(Charsets.US_ASCII)


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
