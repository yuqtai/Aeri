package com.aeri.remote.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.TextUnit
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

@Composable
fun SectionLabel(text: String, modifier: Modifier = Modifier) {
    Text(text, modifier.padding(start = 4.dp), fontSize = 12.sp, fontWeight = FontWeight.SemiBold, color = pal.ink3)
}

@Composable
fun Card(modifier: Modifier = Modifier, content: @Composable ColumnScope.() -> Unit) {
    Column(
        modifier
            .fillMaxWidth()
            .clip(RoundedCornerShape(14.dp))
            .background(pal.surface)
            .border(1.dp, pal.hairline, RoundedCornerShape(14.dp)),
        content = content,
    )
}

@Composable
fun Divider() {
    Box(Modifier.padding(start = 16.dp).fillMaxWidth().height(1.dp).background(pal.hairline))
}

/** 输入框：比卡片略深（“凹进去”），没有边框 */
@Composable
fun Field(
    value: String,
    onValueChange: (String) -> Unit,
    placeholder: String,
    modifier: Modifier = Modifier,
    textStyle: TextStyle = TextStyle(fontSize = 15.sp),
    keyboardOptions: KeyboardOptions = KeyboardOptions.Default,
    keyboardActions: KeyboardActions = KeyboardActions.Default,
    singleLine: Boolean = true,
    enabled: Boolean = true,
) {
    val style = textStyle.copy(color = pal.ink)
    BasicTextField(
        value = value,
        onValueChange = onValueChange,
        modifier = modifier,
        enabled = enabled,
        textStyle = style,
        singleLine = singleLine,
        maxLines = if (singleLine) 1 else 5,
        keyboardOptions = keyboardOptions,
        keyboardActions = keyboardActions,
        cursorBrush = SolidColor(pal.signal),
        decorationBox = { inner ->
            Box(
                Modifier
                    .clip(RoundedCornerShape(12.dp))
                    .background(pal.inset)
                    .padding(horizontal = 14.dp, vertical = 12.dp),
                contentAlignment = Alignment.CenterStart,
            ) {
                // 占位文字不跟着输入内容加字间距（配对码输入框）
                if (value.isEmpty()) Text(placeholder, style = style.copy(color = pal.ink4, letterSpacing = TextUnit.Unspecified))
                inner()
            }
        },
    )
}

/** kind：0 主按钮（强调色）  1 次按钮  2 文字按钮 */
@Composable
fun Btn(
    text: String,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    kind: Int = 1,
    enabled: Boolean = true,
    busy: Boolean = false,
    height: Dp = 44.dp,
) {
    val bg = when {
        !enabled && kind != 2 -> pal.inset
        kind == 0 -> pal.signal
        kind == 1 -> pal.inset
        else -> Color.Transparent
    }
    val fg = when {
        !enabled -> pal.ink4
        kind == 0 -> Color.White
        kind == 2 -> pal.signalInk
        else -> pal.ink
    }
    Row(
        modifier
            .height(height)
            .clip(RoundedCornerShape(12.dp))
            .background(bg)
            .clickable(enabled = enabled && !busy, onClick = onClick)
            .padding(horizontal = 16.dp),
        horizontalArrangement = Arrangement.spacedBy(8.dp, Alignment.CenterHorizontally),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        if (busy) CircularProgressIndicator(Modifier.size(16.dp), color = fg, strokeWidth = 2.dp)
        Text(text, color = fg, fontSize = 15.sp, fontWeight = FontWeight.Medium)
    }
}

/** 圆形图标按钮 */
@Composable
fun RoundIcon(
    icon: ImageVector,
    description: String,
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    size: Dp = 40.dp,
    tint: Color = pal.ink2,
    background: Color = Color.Transparent,
    enabled: Boolean = true,
) {
    Box(
        modifier
            .size(size)
            .clip(CircleShape)
            .background(background)
            .clickable(enabled = enabled, onClick = onClick),
        contentAlignment = Alignment.Center,
    ) {
        Icon(icon, description, Modifier.size(22.dp), tint = if (enabled) tint else pal.ink4)
    }
}

@Composable
fun Dot(color: Color, size: Dp = 8.dp) {
    Box(Modifier.size(size).clip(CircleShape).background(color))
}

/** 小胶囊：状态、标签 */
@Composable
fun Pill(text: String, fg: Color, bg: Color, dot: Color? = null, modifier: Modifier = Modifier) {
    Row(
        modifier
            .clip(RoundedCornerShape(50))
            .background(bg)
            .padding(horizontal = 10.dp, vertical = 5.dp),
        horizontalArrangement = Arrangement.spacedBy(6.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        if (dot != null) Dot(dot, 7.dp)
        Text(text, color = fg, fontSize = 12.sp, fontWeight = FontWeight.Medium)
    }
}
