package com.aeri.remote

import android.content.Context
import androidx.core.content.edit

/** 电脑上 Aeri 手机控制的默认端口（WebSocket 和局域网发现共用） */
const val DEFAULT_PORT = 47821

/** 配对过的电脑 */
data class SavedPc(val host: String, val port: Int, val token: String, val name: String, val id: String)

/** 局域网里找到的电脑（还没配对） */
data class FoundPc(val host: String, val port: Int, val name: String, val id: String)

/** 一条对话。role：0 用户  1 助手  2 工具/任务进展  3 系统提示；task 不为空时是后台任务进展，同一任务紧挨着的进展原地替换 */
data class Chat(val role: Int, val text: String, val task: Int? = null)

enum class Link { Offline, Connecting, Online }

/** 电脑上带的、比手机上新的 App 版本 */
data class AppUpdate(val versionCode: Long, val versionName: String, val size: Long)

/** 和电脑上的状态码一致 */
object Status {
    const val IDLE = 0
    const val LISTENING = 1
    const val THINKING = 2
    const val ACTING = 3
    const val SPEAKING = 4
    const val PAUSED = 5

    fun busy(status: Int) = status in LISTENING..SPEAKING

    fun label(status: Int) = when (status) {
        LISTENING -> "正在听"
        THINKING -> "思考中"
        ACTING -> "正在操作"
        SPEAKING -> "正在回答"
        PAUSED -> "语音唤醒已暂停"
        else -> "待机"
    }
}

/** 配对信息存在本机（令牌只在这台手机和那台电脑之间有效） */
object Store {
    private fun prefs(ctx: Context) = ctx.getSharedPreferences("aeri", Context.MODE_PRIVATE)

    fun load(ctx: Context): SavedPc? {
        val p = prefs(ctx)
        val token = p.getString("token", null) ?: return null
        val host = p.getString("host", null) ?: return null
        return SavedPc(host, p.getInt("port", DEFAULT_PORT), token, p.getString("name", null) ?: host, p.getString("id", "") ?: "")
    }

    fun save(ctx: Context, pc: SavedPc) = prefs(ctx).edit {
        putString("host", pc.host)
        putInt("port", pc.port)
        putString("token", pc.token)
        putString("name", pc.name)
        putString("id", pc.id)
    }

    fun clear(ctx: Context) = prefs(ctx).edit { clear() }
}
