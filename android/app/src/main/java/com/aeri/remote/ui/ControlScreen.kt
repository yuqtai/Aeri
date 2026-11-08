package com.aeri.remote.ui

import android.Manifest
import android.content.pm.PackageManager
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.animation.animateColorAsState
import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.itemsIndexed
import androidx.compose.foundation.lazy.rememberLazyListState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.scale
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.hapticfeedback.HapticFeedbackType
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.LocalHapticFeedback
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.core.content.ContextCompat
import com.aeri.remote.Chat
import com.aeri.remote.Link
import com.aeri.remote.MainViewModel
import com.aeri.remote.Status
import com.aeri.remote.UiState

private val EXAMPLES = listOf("现在几点了", "打开计算器", "音量调小一点", "在 B 站搜索 原神")

@Composable
fun ControlScreen(vm: MainViewModel, ui: UiState, modifier: Modifier = Modifier) {
    Column(modifier.fillMaxSize()) {
        TopBar(vm, ui)
        StatusRow(vm, ui)
        if (ui.appUpdate != null && ui.link == Link.Online) UpdateBanner(vm, ui)
        ChatList(vm, ui, Modifier.weight(1f))
        MediaRow(vm, ui)
        BottomBar(vm, ui)
    }
}

// ---------- 顶栏：电脑名 + 连接状态 ----------

@Composable
private fun TopBar(vm: MainViewModel, ui: UiState) {
    var menu by remember { mutableStateOf(false) }
    var confirmUnpair by remember { mutableStateOf(false) }
    Row(
        Modifier.fillMaxWidth().padding(start = 20.dp, end = 8.dp, top = 10.dp, bottom = 2.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Column(Modifier.weight(1f)) {
            Text(ui.saved?.name ?: "电脑", fontSize = 18.sp, fontWeight = FontWeight.SemiBold, color = pal.ink, maxLines = 1)
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                val (color, text) = when (ui.link) {
                    Link.Online -> pal.ok to "已连接"
                    Link.Connecting -> pal.warn to "正在连接…"
                    Link.Offline -> pal.danger to if (ui.failures > 0) "连不上电脑，正在重试" else "未连接"
                }
                Dot(color, 7.dp)
                Text(text, fontSize = 12.sp, color = pal.ink3)
            }
        }
        if (ui.link == Link.Offline) Btn("重连", { vm.reconnectNow() }, kind = 2, height = 36.dp)
        Box {
            RoundIcon(AeriIcons.more, "更多", { menu = true })
            DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
                DropdownMenuItem(text = { Text("让电脑用自己的麦克风听") }, onClick = {
                    menu = false
                    vm.pcListen()
                })
                DropdownMenuItem(text = { Text("重新配对") }, onClick = {
                    menu = false
                    confirmUnpair = true
                })
                DropdownMenuItem(
                    text = { Text("App 版本 v${ui.appVersion}", color = pal.ink3) },
                    onClick = {},
                    enabled = false,
                )
            }
        }
    }
    if (confirmUnpair) {
        AlertDialog(
            onDismissRequest = { confirmUnpair = false },
            title = { Text("重新配对？") },
            text = { Text("这台手机会忘掉 ${ui.saved?.name ?: "电脑"}，需要在电脑上重新拿配对码。") },
            confirmButton = {
                TextButton(onClick = {
                    confirmUnpair = false
                    vm.unpair()
                }) { Text("重新配对") }
            },
            dismissButton = { TextButton(onClick = { confirmUnpair = false }) { Text("取消") } },
        )
    }
}

// ---------- 状态：待机 / 正在听 / 思考中…，忙的时候可以停止 ----------

@Composable
private fun StatusRow(vm: MainViewModel, ui: UiState) {
    val online = ui.link == Link.Online
    val busy = online && Status.busy(ui.status)
    Row(
        Modifier.fillMaxWidth().height(48.dp).padding(horizontal = 16.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        if (online) {
            val pulse by rememberInfiniteTransition(label = "pulse").animateFloat(
                initialValue = 0.35f,
                targetValue = 1f,
                animationSpec = infiniteRepeatable(tween(700), RepeatMode.Reverse),
                label = "pulse",
            )
            val dot = when {
                busy -> pal.signal.copy(alpha = pulse)
                ui.status == Status.PAUSED -> pal.ink4
                else -> pal.ok
            }
            Pill(Status.label(ui.status), if (busy) pal.signalInk else pal.ink2, if (busy) pal.signalSoft else pal.inset, dot)
            if (ui.taskRunning) Pill("后台任务进行中", pal.ok, pal.okSoft)
        }
        Spacer(Modifier.weight(1f))
        if (busy) {
            Row(
                Modifier
                    .clip(RoundedCornerShape(50))
                    .background(pal.inset)
                    .clickable { vm.stop() }
                    .padding(horizontal = 12.dp, vertical = 6.dp),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.spacedBy(4.dp),
            ) {
                Icon(AeriIcons.stop, null, Modifier.size(16.dp), tint = pal.ink)
                Text("停止", fontSize = 13.sp, fontWeight = FontWeight.Medium, color = pal.ink)
            }
        }
    }
}

// ---------- 新版 App：电脑上带的比手机上的新 ----------

@Composable
private fun UpdateBanner(vm: MainViewModel, ui: UiState) {
    val update = ui.appUpdate ?: return
    val progress = ui.appDownload
    Row(
        Modifier
            .fillMaxWidth()
            .padding(horizontal = 16.dp, vertical = 4.dp)
            .clip(RoundedCornerShape(12.dp))
            .background(pal.signalSoft)
            .padding(start = 14.dp, end = 8.dp, top = 8.dp, bottom = 8.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        Column(Modifier.weight(1f)) {
            Text("电脑上有新版 App：v${update.versionName}", fontSize = 14.sp, fontWeight = FontWeight.Medium, color = pal.signalInk)
            Text(
                if (progress == null) {
                    "%.1f MB，从电脑直接传过来，不走外网".format(update.size / 1048576.0)
                } else {
                    "正在从电脑下载… ${(progress * 100).toInt()}%"
                },
                fontSize = 12.sp,
                color = pal.ink3,
            )
        }
        if (progress == null) {
            Btn("更新", { vm.startAppUpdate() }, kind = 0, height = 36.dp)
        } else {
            CircularProgressIndicator(
                progress = { progress },
                modifier = Modifier.size(26.dp),
                color = pal.signal,
                strokeWidth = 3.dp,
                trackColor = pal.inset,
            )
            Spacer(Modifier.width(4.dp))
        }
    }
}

// ---------- 对话 ----------

@Composable
private fun ChatList(vm: MainViewModel, ui: UiState, modifier: Modifier) {
    if (ui.chats.isEmpty() && ui.partial.isBlank()) {
        EmptyState(vm, ui, modifier)
        return
    }
    val state = rememberLazyListState()
    val count = ui.chats.size + if (ui.partial.isBlank()) 0 else 1
    LaunchedEffect(count, ui.chats.lastOrNull(), ui.partial) {
        if (count > 0) state.animateScrollToItem(count - 1)
    }
    LazyColumn(
        modifier.fillMaxWidth(),
        state = state,
        contentPadding = androidx.compose.foundation.layout.PaddingValues(horizontal = 16.dp, vertical = 8.dp),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        itemsIndexed(ui.chats) { _, chat -> ChatRow(chat) }
        if (ui.partial.isNotBlank()) {
            item { UserBubble(ui.partial + "…", partial = true) }
        }
    }
}

@Composable
private fun ChatRow(chat: Chat) {
    when (chat.role) {
        0 -> UserBubble(chat.text)
        1 -> Text(chat.text, Modifier.fillMaxWidth().padding(end = 24.dp), fontSize = 15.sp, lineHeight = 23.sp, color = pal.ink)
        2 -> Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Dot(pal.ink4, 5.dp)
            Text(chat.text, fontSize = 13.sp, lineHeight = 18.sp, color = pal.ink3)
        }

        else -> Text(
            chat.text,
            Modifier.fillMaxWidth().padding(horizontal = 24.dp),
            fontSize = 12.sp,
            lineHeight = 17.sp,
            color = pal.ink3,
            textAlign = TextAlign.Center,
        )
    }
}

@Composable
private fun UserBubble(text: String, partial: Boolean = false) {
    Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.End) {
        Text(
            text,
            Modifier
                .widthIn(max = 300.dp)
                .clip(RoundedCornerShape(topStart = 18.dp, topEnd = 18.dp, bottomStart = 18.dp, bottomEnd = 6.dp))
                .background(if (partial) pal.signalSoft else pal.surface)
                .padding(horizontal = 14.dp, vertical = 9.dp),
            fontSize = 15.sp,
            lineHeight = 22.sp,
            color = if (partial) pal.signalInk else pal.ink,
        )
    }
}

@OptIn(androidx.compose.foundation.layout.ExperimentalLayoutApi::class)
@Composable
private fun EmptyState(vm: MainViewModel, ui: UiState, modifier: Modifier) {
    Column(
        modifier.fillMaxWidth().padding(horizontal = 32.dp),
        verticalArrangement = Arrangement.Center,
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Glow(64)
        Spacer(Modifier.height(20.dp))
        Text(
            if (ui.link == Link.Online) "按住下面的按钮说话，或者打字" else "连上电脑后就能用了",
            fontSize = 16.sp,
            fontWeight = FontWeight.Medium,
            color = pal.ink,
            textAlign = TextAlign.Center,
        )
        Spacer(Modifier.height(6.dp))
        Text("回答从电脑上播放，这里同步显示对话", fontSize = 13.sp, color = pal.ink3, textAlign = TextAlign.Center)
        if (ui.link == Link.Online) {
            Spacer(Modifier.height(20.dp))
            FlowRow(
                horizontalArrangement = Arrangement.spacedBy(8.dp, Alignment.CenterHorizontally),
                verticalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                EXAMPLES.forEach { example ->
                    Text(
                        example,
                        Modifier
                            .clip(RoundedCornerShape(50))
                            .background(pal.surface)
                            .clickable { vm.sendText(example) }
                            .padding(horizontal = 14.dp, vertical = 8.dp),
                        fontSize = 13.sp,
                        color = pal.ink2,
                    )
                }
            }
        }
    }
}

// ---------- 唤醒开关 + 音量 + 播放控制 ----------

@Composable
private fun MediaRow(vm: MainViewModel, ui: UiState) {
    val online = ui.link == Link.Online
    Row(
        Modifier.fillMaxWidth().padding(horizontal = 12.dp, vertical = 4.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.SpaceBetween,
    ) {
        Row(
            Modifier
                .clip(RoundedCornerShape(50))
                .background(if (ui.listenOn && online) pal.signalSoft else pal.inset)
                .clickable(enabled = online) { vm.setListening(!ui.listenOn) }
                .padding(horizontal = 12.dp, vertical = 8.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.spacedBy(5.dp),
        ) {
            val on = ui.listenOn && online
            Icon(if (ui.listenOn) AeriIcons.mic else AeriIcons.micOff, null, Modifier.size(18.dp), tint = if (on) pal.signal else pal.ink3)
            Text("唤醒", fontSize = 13.sp, fontWeight = FontWeight.Medium, color = if (on) pal.signalInk else pal.ink3)
        }
        RoundIcon(AeriIcons.volumeDown, "音量减小", { vm.media("volume_down") }, enabled = online)
        RoundIcon(AeriIcons.volumeUp, "音量增大", { vm.media("volume_up") }, enabled = online)
        RoundIcon(AeriIcons.mute, "静音", { vm.media("mute") }, enabled = online)
        RoundIcon(AeriIcons.previous, "上一首", { vm.media("previous") }, enabled = online)
        RoundIcon(AeriIcons.playPause, "播放或暂停", { vm.media("play_pause") }, enabled = online)
        RoundIcon(AeriIcons.next, "下一首", { vm.media("next") }, enabled = online)
    }
}

// ---------- 底栏：按住说话 / 打字 ----------

@Composable
private fun BottomBar(vm: MainViewModel, ui: UiState) {
    var typing by rememberSaveable { mutableStateOf(false) }
    var text by rememberSaveable { mutableStateOf("") }
    val online = ui.link == Link.Online
    val send = {
        if (vm.sendText(text)) text = ""
    }
    Row(
        Modifier
            .fillMaxWidth()
            .background(pal.surface)
            .padding(horizontal = 12.dp, vertical = 10.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        RoundIcon(
            if (typing) AeriIcons.mic else AeriIcons.keyboard,
            if (typing) "切换到按住说话" else "切换到打字",
            { typing = !typing },
            size = 46.dp,
            background = pal.inset,
        )
        if (typing) {
            Field(
                text,
                { text = it },
                "输入要电脑做的事",
                Modifier.weight(1f),
                keyboardOptions = KeyboardOptions(imeAction = ImeAction.Send),
                keyboardActions = KeyboardActions(onSend = { send() }),
                singleLine = false,
            )
            RoundIcon(
                AeriIcons.send,
                "发送",
                { send() },
                size = 46.dp,
                tint = Color.White,
                background = if (online && text.isNotBlank()) pal.signal else pal.insetStrong,
                enabled = online && text.isNotBlank(),
            )
        } else {
            TalkButton(vm, ui.talking, online, Modifier.weight(1f))
        }
    }
}

/** 按住说话，松开结束（像发语音消息一样）。声音从手机麦克风实时传给电脑 */
@Composable
private fun TalkButton(vm: MainViewModel, talking: Boolean, online: Boolean, modifier: Modifier) {
    val context = LocalContext.current
    val haptic = LocalHapticFeedback.current
    val permission = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
        if (!granted) vm.notifyMicDenied()
    }
    val bg by animateColorAsState(if (talking) pal.signal else pal.inset, label = "talk-bg")
    val scale by animateFloatAsState(if (talking) 0.97f else 1f, label = "talk-scale")
    val glow by rememberInfiniteTransition(label = "talk-glow").animateFloat(
        initialValue = 0.6f,
        targetValue = 1f,
        animationSpec = infiniteRepeatable(tween(600), RepeatMode.Reverse),
        label = "talk-glow",
    )
    Box(
        modifier
            .height(52.dp)
            .scale(scale)
            .clip(RoundedCornerShape(26.dp))
            .background(bg)
            .pointerInput(online) {
                detectTapGestures(onPress = {
                    if (!online) {
                        vm.startTalking() // 会提示“还没连上电脑”
                        return@detectTapGestures
                    }
                    val granted = ContextCompat.checkSelfPermission(context, Manifest.permission.RECORD_AUDIO) ==
                        PackageManager.PERMISSION_GRANTED
                    if (!granted) {
                        permission.launch(Manifest.permission.RECORD_AUDIO)
                        return@detectTapGestures
                    }
                    if (vm.startTalking()) {
                        haptic.performHapticFeedback(HapticFeedbackType.LongPress)
                        tryAwaitRelease()
                        vm.stopTalking() // 连接中途断了时已经停过，这里什么也不做
                    }
                })
            },
        contentAlignment = Alignment.Center,
    ) {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            if (talking) {
                Box(Modifier.size(8.dp).clip(CircleShape).background(Color.White.copy(alpha = glow)))
            } else {
                Icon(AeriIcons.mic, null, Modifier.size(20.dp), tint = if (online) pal.ink2 else pal.ink4)
            }
            Text(
                if (talking) "松开 结束" else "按住 说话",
                fontSize = 16.sp,
                fontWeight = FontWeight.Medium,
                color = if (talking) Color.White else if (online) pal.ink else pal.ink4,
            )
            if (talking) Spacer(Modifier.width(8.dp))
        }
    }
}
