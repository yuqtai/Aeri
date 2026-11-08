package com.aeri.remote.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.aeri.remote.FoundPc
import com.aeri.remote.MainViewModel

/** 还没配对：在局域网里找到电脑，输入电脑上显示的 6 位配对码 */
@Composable
fun PairScreen(vm: MainViewModel, modifier: Modifier = Modifier) {
    val pair by vm.pair.collectAsStateWithLifecycle()
    var selectedHost by rememberSaveable { mutableStateOf<String?>(null) }
    var manual by rememberSaveable { mutableStateOf("") }
    var code by rememberSaveable { mutableStateOf("") }
    // 只找到一台时直接选上
    LaunchedEffect(pair.found) {
        if (selectedHost == null && pair.found.size == 1) selectedHost = pair.found[0].host
    }
    val target: FoundPc? = vm.manualTarget(manual) ?: pair.found.firstOrNull { it.host == selectedHost }
    val submit = { target?.let { vm.pairWith(it, code) } }

    Column(
        modifier
            .fillMaxSize()
            .verticalScroll(rememberScrollState())
            .padding(horizontal = 20.dp, vertical = 24.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
            Glow()
            Column {
                Text("连接电脑上的 Aeri", fontSize = 22.sp, fontWeight = FontWeight.SemiBold, color = pal.ink)
                Text("在手机上打字、按住说话、调音量", fontSize = 13.sp, color = pal.ink3)
            }
        }
        Spacer(Modifier.size(4.dp))
        Card {
            Text(
                "手机和电脑连同一个 Wi-Fi。在电脑 Aeri 的 设置 → 手机控制 里打开“允许手机控制”并保存，再点“配对新手机”。",
                Modifier.padding(16.dp),
                fontSize = 14.sp,
                lineHeight = 21.sp,
                color = pal.ink2,
            )
        }

        Row(Modifier.fillMaxWidth().padding(top = 8.dp), verticalAlignment = Alignment.CenterVertically) {
            SectionLabel("找到的电脑", Modifier.weight(1f))
            if (pair.searching) {
                CircularProgressIndicator(Modifier.size(16.dp), color = pal.ink3, strokeWidth = 2.dp)
                Spacer(Modifier.width(12.dp))
            } else {
                RoundIcon(AeriIcons.refresh, "重新查找", { vm.search() }, size = 32.dp)
            }
        }
        Card {
            when {
                pair.found.isEmpty() && (pair.searching || !pair.searched) -> Hint("正在局域网里查找…")
                pair.found.isEmpty() -> Hint("没找到电脑。确认电脑上打开了手机控制、两边连的是同一个 Wi-Fi；也可以在下面直接填电脑地址。")
                else -> pair.found.forEachIndexed { i, pc ->
                    if (i > 0) Divider()
                    PcRow(pc, selected = manual.isBlank() && pc.host == selectedHost) {
                        selectedHost = pc.host
                        manual = ""
                    }
                }
            }
        }

        SectionLabel("或者手动填地址", Modifier.padding(top = 8.dp))
        Field(
            manual,
            { manual = it.filter { c -> c.isDigit() || c == '.' || c == ':' } },
            "电脑设置页显示的地址，例如 192.168.1.8",
            Modifier.fillMaxWidth(),
            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Uri, imeAction = ImeAction.Next),
        )

        SectionLabel("配对码", Modifier.padding(top = 8.dp))
        Field(
            code,
            { code = it.filter(Char::isDigit).take(6) },
            "6 位数字",
            Modifier.fillMaxWidth(),
            textStyle = TextStyle(fontSize = 24.sp, fontWeight = FontWeight.SemiBold, letterSpacing = 6.sp),
            keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.NumberPassword, imeAction = ImeAction.Done),
            keyboardActions = KeyboardActions(onDone = { submit() }),
        )
        pair.error?.let {
            Text(it, fontSize = 13.sp, lineHeight = 19.sp, color = pal.danger, modifier = Modifier.padding(horizontal = 4.dp))
        }
        Btn(
            "配对",
            { submit() },
            Modifier.fillMaxWidth().padding(top = 8.dp),
            kind = 0,
            enabled = target != null && code.length == 6,
            busy = pair.busy,
            height = 50.dp,
        )
        Text(
            "配对后这台手机会记住电脑，电脑换了 IP 也能自动找到。",
            Modifier.fillMaxWidth(),
            fontSize = 12.sp,
            color = pal.ink4,
            textAlign = TextAlign.Center,
        )
    }
}

@Composable
private fun Hint(text: String) {
    Text(text, Modifier.padding(16.dp), fontSize = 14.sp, lineHeight = 21.sp, color = pal.ink3)
}

@Composable
private fun PcRow(pc: FoundPc, selected: Boolean, onClick: () -> Unit) {
    Row(
        Modifier
            .fillMaxWidth()
            .clickable(onClick = onClick)
            .padding(horizontal = 16.dp, vertical = 14.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        Box(
            Modifier.size(36.dp).clip(RoundedCornerShape(10.dp)).background(if (selected) pal.signalSoft else pal.inset),
            contentAlignment = Alignment.Center,
        ) {
            Icon(AeriIcons.desktop, null, Modifier.size(20.dp), tint = if (selected) pal.signal else pal.ink3)
        }
        Column(Modifier.weight(1f)) {
            Text(pc.name, fontSize = 15.sp, fontWeight = FontWeight.Medium, color = pal.ink)
            Text(pc.host, fontSize = 12.sp, color = pal.ink3)
        }
        if (selected) Dot(pal.signal)
    }
}

/** 品牌光球：和电脑上的“聆听光球”同一种光 */
@Composable
fun Glow(size: Int = 44) {
    Box(
        Modifier
            .size(size.dp)
            .clip(CircleShape)
            .background(Brush.radialGradient(listOf(pal.signal.copy(alpha = 0.95f), pal.signal.copy(alpha = 0.35f), pal.signalSoft))),
    )
}
