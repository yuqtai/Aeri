package com.aeri.remote

import android.annotation.SuppressLint
import android.media.AudioFormat
import android.media.AudioRecord
import android.media.MediaRecorder
import kotlin.concurrent.thread

/** 手机麦克风：16kHz 单声道 16 位 PCM，每 40 毫秒交出一块（和电脑上识别用的格式一样，电脑不用再转换） */
class Mic {
    @Volatile
    private var running = false
    private var worker: Thread? = null

    /** 开始录音；打不开（被占用、没权限）时返回 false。调用前要确认已经有录音权限 */
    @SuppressLint("MissingPermission")
    fun start(onChunk: (ByteArray) -> Unit): Boolean {
        stop()
        val min = AudioRecord.getMinBufferSize(RATE, AudioFormat.CHANNEL_IN_MONO, AudioFormat.ENCODING_PCM_16BIT)
        if (min <= 0) return false
        val recorder = runCatching {
            // VOICE_RECOGNITION：系统按“给语音识别用”调校（不做通话那样的强压缩）
            AudioRecord(
                MediaRecorder.AudioSource.VOICE_RECOGNITION,
                RATE,
                AudioFormat.CHANNEL_IN_MONO,
                AudioFormat.ENCODING_PCM_16BIT,
                maxOf(min, CHUNK_BYTES * 4),
            )
        }.getOrNull() ?: return false
        if (recorder.state != AudioRecord.STATE_INITIALIZED) {
            recorder.release()
            return false
        }
        runCatching { recorder.startRecording() }.onFailure {
            recorder.release()
            return false
        }
        running = true
        worker = thread(name = "mic") {
            val buf = ByteArray(CHUNK_BYTES)
            try {
                while (running) {
                    val n = recorder.read(buf, 0, buf.size)
                    if (n > 0) onChunk(buf.copyOf(n)) else if (n < 0) break
                }
            } finally {
                runCatching { recorder.stop() }
                recorder.release()
            }
        }
        return true
    }

    /** 停止录音，等最后一块交出去再返回 */
    fun stop() {
        running = false
        worker?.join(1000)
        worker = null
    }

    private companion object {
        const val RATE = 16000
        const val CHUNK_BYTES = RATE / 25 * 2 // 40 毫秒
    }
}
