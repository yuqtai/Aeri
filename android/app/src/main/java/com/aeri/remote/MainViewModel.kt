package com.aeri.remote

import android.app.Application
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.SystemClock
import android.provider.Settings
import androidx.core.content.FileProvider
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharedFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import okhttp3.OkHttpClient
import okhttp3.Request
import okhttp3.Response
import okhttp3.WebSocket
import okhttp3.WebSocketListener
import okio.ByteString
import okio.ByteString.Companion.toByteString
import org.json.JSONObject
import java.io.File
import java.io.FileOutputStream
import java.security.MessageDigest
import java.util.concurrent.TimeUnit
import kotlin.math.min

data class UiState(
    val saved: SavedPc? = null,
    val link: Link = Link.Offline,
    val status: Int = Status.IDLE,
    /** 电脑上的语音唤醒开着 */
    val listenOn: Boolean = true,
    /** 正在识别的半句话 */
    val partial: String = "",
    val taskRunning: Boolean = false,
    val chats: List<Chat> = emptyList(),
    /** 正按着说话键 */
    val talking: Boolean = false,
    /** 连续连不上的次数 */
    val failures: Int = 0,
    /** 手机上装的 App 版本 */
    val appVersion: String = "",
    /** 电脑上有更新的 App 时不为空 */
    val appUpdate: AppUpdate? = null,
    /** 正在从电脑下载新版 App：进度 0..1；没在下载时为空 */
    val appDownload: Float? = null,
)

data class PairUi(
    val searching: Boolean = false,
    val searched: Boolean = false,
    val found: List<FoundPc> = emptyList(),
    val busy: Boolean = false,
    val error: String? = null,
)

/**
 * 和电脑上 Aeri 的连接（协议见 android/PROTOCOL.md）。
 * 界面在前台时保持连接，断了自动重连；连续连不上时先在局域网里按编号重新找电脑（IP 可能变了）。
 */
class MainViewModel(app: Application) : AndroidViewModel(app) {
    /** 手机上装的 App 的版本号（和电脑版一致，见 build.gradle.kts） */
    private val installedCode: Long
    private val installedName: String

    init {
        val info = app.packageManager.getPackageInfo(app.packageName, 0)
        @Suppress("DEPRECATION")
        installedCode = if (Build.VERSION.SDK_INT >= 28) info.longVersionCode else info.versionCode.toLong()
        installedName = info.versionName.orEmpty()
    }

    private val http = OkHttpClient.Builder()
        .connectTimeout(4, TimeUnit.SECONDS)
        .readTimeout(0, TimeUnit.MILLISECONDS)
        .pingInterval(15, TimeUnit.SECONDS)
        .build()

    private val _ui = MutableStateFlow(UiState(saved = Store.load(app), appVersion = installedName))
    val ui: StateFlow<UiState> = _ui
    private val _pair = MutableStateFlow(PairUi())
    val pair: StateFlow<PairUi> = _pair
    private val _notices = MutableSharedFlow<String>(extraBufferCapacity = 8)

    /** 一次性的提示 */
    val notices: SharedFlow<String> = _notices

    private var socket: WebSocket? = null

    /** 每开一个新连接加一；旧连接迟到的回调一律作废（收消息的线程也会读） */
    @Volatile
    private var generation = 0
    private var foreground = false
    private var retry: Job? = null
    private var searchJob: Job? = null

    /** 正在配对的电脑（还没拿到令牌） */
    private var pairing: FoundPc? = null
    private val mic = Mic()
    private var talkStarted = 0L

    private fun notice(text: String) {
        _notices.tryEmit(text)
    }

    // ---------- 前后台 ----------

    fun onStart() {
        foreground = true
        if (_ui.value.saved != null) connect() else search()
    }

    fun onStop() {
        foreground = false
        stopTalking(cancel = true)
        disconnect()
    }

    // ---------- 连接 ----------

    private fun open(host: String, port: Int, first: JSONObject) {
        retry?.cancel()
        retry = null
        socket?.cancel()
        val gen = ++generation
        _ui.update { it.copy(link = Link.Connecting) }
        val request = Request.Builder().url("ws://$host:$port/").build()
        socket = http.newWebSocket(request, object : WebSocketListener() {
            override fun onOpen(webSocket: WebSocket, response: Response) {
                webSocket.send(first.toString())
            }

            override fun onMessage(webSocket: WebSocket, text: String) {
                val msg = runCatching { JSONObject(text) }.getOrNull() ?: return
                if (gen != generation) return
                when (msg.optString("t")) {
                    // 下载新版 App 的消息就在收消息的线程里按顺序处理，和二进制块不会乱序
                    "app_begin", "app_end", "app_error" -> appMessage(msg)
                    else -> onMain { if (gen == generation) handle(msg) }
                }
            }

            override fun onMessage(webSocket: WebSocket, bytes: ByteString) {
                if (gen == generation) appChunk(bytes)
            }

            override fun onClosing(webSocket: WebSocket, code: Int, reason: String) {
                webSocket.close(1000, null)
            }

            override fun onClosed(webSocket: WebSocket, code: Int, reason: String) {
                onMain { if (gen == generation) lost() }
            }

            override fun onFailure(webSocket: WebSocket, t: Throwable, response: Response?) {
                onMain { if (gen == generation) lost() }
            }
        })
    }

    private fun onMain(block: () -> Unit) {
        viewModelScope.launch(Dispatchers.Main.immediate) { block() }
    }

    private fun connect() {
        val pc = _ui.value.saved ?: return
        if (!foreground) return
        open(pc.host, pc.port, JSONObject().put("t", "hello").put("token", pc.token))
    }

    /** 点了“重连” */
    fun reconnectNow() {
        _ui.update { it.copy(failures = 0) }
        connect()
    }

    private fun disconnect() {
        retry?.cancel()
        retry = null
        generation++
        abortDownload(null)
        socket?.close(1000, null)
        socket = null
        _ui.update { it.copy(link = Link.Offline) }
    }

    /** 连接断了：配对中就报错；否则过一会儿重连 */
    private fun lost() {
        socket = null
        dropTalking()
        abortDownload("连接断了，新版 App 没下完，连上后再点一次“更新”")
        val target = pairing
        if (target != null) {
            pairing = null
            _ui.update { it.copy(link = Link.Offline) }
            _pair.update {
                it.copy(busy = false, error = "连不上 ${target.host}：确认电脑上打开了手机控制，手机和电脑连的是同一个 Wi-Fi")
            }
            return
        }
        val failures = _ui.value.failures + 1
        _ui.update { it.copy(link = Link.Offline, failures = failures) }
        if (!foreground || _ui.value.saved == null) return
        retry = viewModelScope.launch {
            if (failures >= 2) relocate()
            delay(min(10_000L, 1000L shl min(failures - 1, 4)))
            retry = null
            connect()
        }
    }

    /** 电脑的 IP 可能变了：按编号在局域网里重新找 */
    private suspend fun relocate() {
        val pc = _ui.value.saved ?: return
        if (pc.id.isEmpty()) return
        val hit = Discovery.search(timeoutMs = 1500, port = pc.port) { it.id == pc.id }.firstOrNull { it.id == pc.id } ?: return
        if (hit.host != pc.host || hit.port != pc.port) {
            val moved = pc.copy(host = hit.host, port = hit.port)
            Store.save(getApplication(), moved)
            _ui.update { it.copy(saved = moved) }
        }
    }

    private fun handle(msg: JSONObject) {
        when (msg.optString("t")) {
            "paired" -> {
                val target = pairing ?: return
                pairing = null
                val pc = SavedPc(
                    host = target.host,
                    port = target.port,
                    token = msg.optString("token"),
                    name = msg.optString("pc", target.name),
                    id = msg.optString("id", target.id),
                )
                Store.save(getApplication(), pc)
                _ui.update { it.copy(saved = pc, chats = emptyList(), failures = 0) }
                _pair.value = PairUi()
            }

            "denied" -> {
                // 服务端随后会断开，不当成掉线重连
                generation++
                socket?.cancel()
                socket = null
                _ui.update { it.copy(link = Link.Offline) }
                if (pairing != null) {
                    pairing = null
                    val error = when (msg.optString("reason")) {
                        "bad_code" -> "配对码不对，再看一下电脑上的 6 位数字"
                        "save_failed" -> "电脑没能保存配对信息，请重试"
                        else -> "电脑上没有在配对，或者配对码过期了：请在电脑的 设置 → 手机控制 里点“配对新手机”"
                    }
                    _pair.update { it.copy(busy = false, error = error) }
                } else {
                    Store.clear(getApplication())
                    _ui.value = UiState(appVersion = installedName)
                    notice("电脑上取消了配对，请重新配对")
                    search()
                }
            }

            "welcome" -> {
                val history = msg.optJSONArray("history")
                val chats = buildList {
                    for (i in 0 until (history?.length() ?: 0)) add(parseChat(history!!.getJSONObject(i)))
                }
                val status = msg.optInt("status")
                // 电脑上带的 App 比手机上的新：提示更新
                val update = msg.optJSONObject("app")?.let {
                    AppUpdate(it.optLong("version_code"), it.optString("version_name"), it.optLong("size"))
                }?.takeIf { it.versionCode > installedCode }
                val old = _ui.value.saved
                val renamed = old?.copy(name = msg.optString("pc", old.name))
                if (renamed != null && renamed != old) Store.save(getApplication(), renamed)
                _ui.update {
                    it.copy(
                        saved = renamed ?: it.saved,
                        link = Link.Online,
                        failures = 0,
                        status = status,
                        listenOn = listenFrom(status, it.listenOn),
                        partial = msg.optString("partial"),
                        taskRunning = msg.optBoolean("task_running"),
                        chats = chats,
                        appUpdate = update,
                    )
                }
            }

            "status" -> {
                val status = msg.optInt("s")
                _ui.update { it.copy(status = status, listenOn = listenFrom(status, it.listenOn)) }
            }

            "partial" -> _ui.update { it.copy(partial = msg.optString("text")) }
            "chat" -> {
                val item = msg.optJSONObject("item") ?: return
                val chat = parseChat(item)
                _ui.update { it.copy(chats = it.chats.withChat(chat)) }
            }

            "task_running" -> _ui.update { it.copy(taskRunning = msg.optBoolean("on")) }
            "clear" -> _ui.update { it.copy(chats = emptyList()) }
        }
    }

    /** 待机时能看出唤醒开没开；忙的时候保持原来的判断 */
    private fun listenFrom(status: Int, previous: Boolean) = when (status) {
        Status.IDLE -> true
        Status.PAUSED -> false
        else -> previous
    }

    private fun parseChat(o: JSONObject) =
        Chat(o.optInt("role"), o.optString("text"), if (o.has("task")) o.optInt("task") else null)

    private fun List<Chat>.withChat(chat: Chat): List<Chat> {
        val last = lastOrNull()
        val next = if (chat.task != null && last?.task == chat.task) dropLast(1) + chat else this + chat
        return if (next.size > MAX_CHATS) next.takeLast(MAX_CHATS) else next
    }

    // ---------- 配对 ----------

    /** 在局域网里找电脑 */
    fun search() {
        if (searchJob?.isActive == true) return
        _pair.update { it.copy(searching = true, error = null) }
        searchJob = viewModelScope.launch {
            val found = Discovery.search()
            _pair.update { it.copy(searching = false, searched = true, found = found) }
        }
    }

    /** 手动填的地址：192.168.1.8 或 192.168.1.8:47821 */
    fun manualTarget(text: String): FoundPc? {
        val t = text.trim()
        if (t.isEmpty()) return null
        val host = t.substringBefore(':').trim()
        if (host.isEmpty()) return null
        val port = t.substringAfter(':', "").trim().toIntOrNull() ?: DEFAULT_PORT
        return FoundPc(host, port, host, "")
    }

    fun pairWith(pc: FoundPc, code: String) {
        if (code.length != 6) {
            _pair.update { it.copy(error = "配对码是 6 位数字") }
            return
        }
        pairing = pc
        _pair.update { it.copy(busy = true, error = null) }
        open(pc.host, pc.port, JSONObject().put("t", "pair").put("code", code).put("name", deviceName()))
    }

    /** 忘掉这台电脑，回到配对页 */
    fun unpair() {
        stopTalking(cancel = true)
        disconnect()
        Store.clear(getApplication())
        _ui.value = UiState(appVersion = installedName)
        _pair.value = PairUi()
        search()
    }

    private fun deviceName(): String {
        val model = Build.MODEL.orEmpty()
        val maker = Build.MANUFACTURER.orEmpty()
        return if (model.startsWith(maker, ignoreCase = true)) model else "$maker $model".trim()
    }

    // ---------- 命令 ----------

    private fun send(json: JSONObject): Boolean {
        val ws = socket
        if (ws == null || _ui.value.link != Link.Online) {
            notice("还没连上电脑")
            return false
        }
        return ws.send(json.toString())
    }

    fun sendText(text: String): Boolean {
        val t = text.trim()
        return t.isNotEmpty() && send(JSONObject().put("t", "text").put("text", t))
    }

    fun stop() {
        send(JSONObject().put("t", "stop"))
    }

    /** 让电脑用它自己的麦克风开始听 */
    fun pcListen() {
        send(JSONObject().put("t", "talk"))
    }

    fun setListening(on: Boolean) {
        if (send(JSONObject().put("t", "listen").put("on", on))) _ui.update { it.copy(listenOn = on) }
    }

    /** volume_up / volume_down / mute / play_pause / next / previous */
    fun media(key: String) {
        send(JSONObject().put("t", "media").put("key", key))
    }

    // ---------- 按住说话 ----------

    /** 按下说话键：手机麦克风的声音实时传给电脑。调用前要确认已经有录音权限 */
    fun startTalking(): Boolean {
        if (_ui.value.talking) return true
        val ws = socket
        if (ws == null || _ui.value.link != Link.Online) {
            notice("还没连上电脑")
            return false
        }
        ws.send(JSONObject().put("t", "ptt").put("on", true).toString())
        talkStarted = SystemClock.elapsedRealtime()
        if (!mic.start { chunk -> ws.send(chunk.toByteString()) }) {
            ws.send(JSONObject().put("t", "ptt").put("on", false).put("cancel", true).toString())
            notice("麦克风打不开，可能被别的应用占用了")
            return false
        }
        _ui.update { it.copy(talking = true) }
        return true
    }

    /** 松开说话键。按得太短（误触）或要取消时，告诉电脑这次不算 */
    fun stopTalking(cancel: Boolean = false) {
        if (!_ui.value.talking) return
        _ui.update { it.copy(talking = false) }
        val ws = socket
        val tooShort = SystemClock.elapsedRealtime() - talkStarted < MIN_TALK_MS
        viewModelScope.launch(Dispatchers.IO) {
            mic.stop() // 等最后一块声音发出去，再说“松开了”
            val msg = JSONObject().put("t", "ptt").put("on", false)
            if (cancel || tooShort) msg.put("cancel", true)
            ws?.send(msg.toString())
        }
        if (tooShort && !cancel) notice("按住说话，说完再松开")
    }

    fun notifyMicDenied() {
        notice("需要麦克风权限才能按住说话：请在系统设置里允许 Aeri 使用麦克风")
    }

    /** 连接断了：录音停掉就行（电脑那边连接断开时会自动松开） */
    private fun dropTalking() {
        if (!_ui.value.talking) return
        _ui.update { it.copy(talking = false) }
        viewModelScope.launch(Dispatchers.IO) { mic.stop() }
    }

    // ---------- App 更新：从电脑下载新版，核对后交给系统安装 ----------

    /** 正在下载的安装包（收消息的线程写，界面线程只在开始和取消时碰） */
    private class Download(val file: File, val size: Long, val sha256: String) {
        private val out = FileOutputStream(file)
        private val digest = MessageDigest.getInstance("SHA-256")
        var received = 0L
        var reported = 0f

        fun write(bytes: ByteString): Boolean = runCatching {
            val b = bytes.toByteArray()
            out.write(b)
            digest.update(b)
            received += b.size
            received <= size
        }.getOrDefault(false)

        /** 写完并核对大小和 SHA-256 */
        fun finish(): Boolean {
            runCatching { out.close() }
            val hex = digest.digest().joinToString("") { "%02x".format(it) }
            return received == size && hex == sha256
        }

        fun abort() {
            runCatching { out.close() }
            file.delete()
        }
    }

    @Volatile
    private var download: Download? = null

    /** 点了“更新”：先确认允许安装应用，再请电脑传过来 */
    fun startAppUpdate() {
        val app = getApplication<Application>()
        if (_ui.value.appDownload != null) return
        if (!app.packageManager.canRequestPackageInstalls()) {
            notice("请允许 Aeri 安装应用，然后回到这里再点一次“更新”")
            val intent = Intent(Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES, Uri.parse("package:${app.packageName}"))
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            runCatching { app.startActivity(intent) }
            return
        }
        if (send(JSONObject().put("t", "get_app"))) _ui.update { it.copy(appDownload = 0f) }
    }

    private fun appMessage(msg: JSONObject) {
        when (msg.optString("t")) {
            "app_begin" -> {
                download?.abort()
                val dir = File(getApplication<Application>().cacheDir, "updates").apply { mkdirs() }
                val started = runCatching {
                    Download(File(dir, "Aeri.apk"), msg.optLong("size"), msg.optString("sha256").lowercase())
                }.getOrNull()
                download = started
                onMain {
                    if (started == null) failDownload("手机上没法保存安装包") else _ui.update { it.copy(appDownload = 0f) }
                }
            }

            "app_end" -> {
                val d = download ?: return
                download = null
                val ok = d.finish()
                if (!ok) d.file.delete()
                onMain {
                    if (ok) {
                        _ui.update { it.copy(appDownload = null) }
                        install(d.file)
                    } else {
                        failDownload("安装包校验没通过，请再试一次")
                    }
                }
            }

            "app_error" -> {
                download?.abort()
                download = null
                onMain { failDownload("电脑上没有找到手机 App 安装包") }
            }
        }
    }

    private fun appChunk(bytes: ByteString) {
        val d = download ?: return
        if (!d.write(bytes)) {
            download = null
            d.abort()
            onMain { failDownload("下载出错了，请再试一次") }
            return
        }
        val progress = d.received.toFloat() / d.size.coerceAtLeast(1)
        if (progress - d.reported >= 0.02f || d.received >= d.size) {
            d.reported = progress
            onMain { _ui.update { if (it.appDownload != null) it.copy(appDownload = progress) else it } }
        }
    }

    private fun failDownload(text: String) {
        _ui.update { it.copy(appDownload = null) }
        notice(text)
    }

    /** 连接断了或离开界面：丢掉没下完的 */
    private fun abortDownload(reason: String?) {
        val d = download
        download = null
        d?.abort()
        if (_ui.value.appDownload != null) {
            _ui.update { it.copy(appDownload = null) }
            if (reason != null) notice(reason)
        }
    }

    /** 打开系统安装界面（安卓要求你在那里点“安装”确认） */
    private fun install(file: File) {
        val app = getApplication<Application>()
        val uri = FileProvider.getUriForFile(app, "${app.packageName}.files", file)
        val intent = Intent(Intent.ACTION_VIEW)
            .setDataAndType(uri, "application/vnd.android.package-archive")
            .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_ACTIVITY_NEW_TASK)
        runCatching { app.startActivity(intent) }.onFailure { notice("打不开系统安装界面：${it.message}") }
    }

    override fun onCleared() {
        mic.stop()
        socket?.cancel()
        http.dispatcher.executorService.shutdown()
    }

    private companion object {
        const val MAX_CHATS = 200
        const val MIN_TALK_MS = 400L
    }
}
