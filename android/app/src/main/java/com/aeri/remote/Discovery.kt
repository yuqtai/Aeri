package com.aeri.remote

import android.os.SystemClock
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.isActive
import kotlinx.coroutines.withContext
import org.json.JSONObject
import java.net.DatagramPacket
import java.net.DatagramSocket
import java.net.InetAddress
import java.net.NetworkInterface
import java.net.SocketTimeoutException

/** 在局域网里广播找电脑：电脑上的 Aeri 收到后回一句自己的名字、编号和端口 */
object Discovery {
    private val REQUEST = "AERI_DISCOVER".toByteArray()

    /** 找 timeoutMs 毫秒，返回找到的电脑；stopWhen 返回 true 时提前结束 */
    suspend fun search(
        timeoutMs: Long = 2500,
        port: Int = DEFAULT_PORT,
        stopWhen: (FoundPc) -> Boolean = { false },
    ): List<FoundPc> = withContext(Dispatchers.IO) {
        val found = LinkedHashMap<String, FoundPc>()
        runCatching {
            DatagramSocket().use { socket ->
                socket.broadcast = true
                socket.soTimeout = 250
                val targets = broadcastTargets()
                val end = SystemClock.elapsedRealtime() + timeoutMs
                var lastSent = 0L
                val buf = ByteArray(1024)
                while (isActive && SystemClock.elapsedRealtime() < end) {
                    val now = SystemClock.elapsedRealtime()
                    // 隔一会儿再喊一次，UDP 丢一两个包也不要紧
                    if (now - lastSent > 800) {
                        lastSent = now
                        for (target in targets) {
                            runCatching { socket.send(DatagramPacket(REQUEST, REQUEST.size, target, port)) }
                        }
                    }
                    val packet = DatagramPacket(buf, buf.size)
                    try {
                        socket.receive(packet)
                    } catch (_: SocketTimeoutException) {
                        continue
                    }
                    val json = runCatching { JSONObject(String(packet.data, 0, packet.length)) }.getOrNull() ?: continue
                    if (json.optString("t") != "aeri") continue
                    val host = packet.address?.hostAddress ?: continue
                    val id = json.optString("id")
                    val pc = FoundPc(host, json.optInt("port", port), json.optString("pc", host), id)
                    found[id.ifEmpty { host }] = pc
                    if (stopWhen(pc)) break
                }
            }
        }
        found.values.toList()
    }

    /** 全网广播 + 每块网卡所在网段的广播地址（有的路由器不转发 255.255.255.255） */
    private fun broadcastTargets(): List<InetAddress> {
        val list = mutableListOf(InetAddress.getByName("255.255.255.255"))
        runCatching {
            for (nif in NetworkInterface.getNetworkInterfaces()) {
                if (!nif.isUp || nif.isLoopback) continue
                for (address in nif.interfaceAddresses) {
                    val broadcast = address.broadcast ?: continue
                    if (broadcast !in list) list += broadcast
                }
            }
        }
        return list
    }
}
