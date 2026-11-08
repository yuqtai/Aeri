package com.aeri.remote.ui

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.Immutable
import androidx.compose.runtime.ReadOnlyComposable
import androidx.compose.runtime.staticCompositionLocalOf
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.StrokeJoin
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.graphics.vector.addPathNodes
import androidx.compose.ui.unit.dp

/** 和电脑版同一套颜色：一种强调色（“聆听光”）只用来表示助手正在工作，其余都是中性色 */
@Immutable
data class Palette(
    val dark: Boolean,
    val canvas: Color,
    val surface: Color,
    val inset: Color,
    val insetStrong: Color,
    val hairline: Color,
    val ink: Color,
    val ink2: Color,
    val ink3: Color,
    val ink4: Color,
    val signal: Color,
    val signalSoft: Color,
    val signalInk: Color,
    val ok: Color,
    val okSoft: Color,
    val warn: Color,
    val danger: Color,
)

private val Light = Palette(
    dark = false,
    canvas = Color(0xFFF4F5F7),
    surface = Color(0xFFFFFFFF),
    inset = Color(0xFFEEF0F3),
    insetStrong = Color(0xFFE2E5EA),
    hairline = Color(0x120F172A),
    ink = Color(0xFF15171B),
    ink2 = Color(0xFF464B55),
    ink3 = Color(0xFF7B808A),
    ink4 = Color(0xFFA9AEB6),
    signal = Color(0xFF4B57F0),
    signalSoft = Color(0xFFECEEFE),
    signalInk = Color(0xFF2E39C4),
    ok = Color(0xFF15803D),
    okSoft = Color(0xFFE8F6ED),
    warn = Color(0xFFB45309),
    danger = Color(0xFFDC2626),
)

private val Dark = Palette(
    dark = true,
    canvas = Color(0xFF0E0F12),
    surface = Color(0xFF16181D),
    inset = Color(0xFF101216),
    insetStrong = Color(0xFF282C33),
    hairline = Color(0x14FFFFFF),
    ink = Color(0xFFECEEF1),
    ink2 = Color(0xFFB6BAC2),
    ink3 = Color(0xFF82878F),
    ink4 = Color(0xFF555A62),
    signal = Color(0xFF7A84FF),
    signalSoft = Color(0x297A84FF),
    signalInk = Color(0xFFC6CAFF),
    ok = Color(0xFF4ADE80),
    okSoft = Color(0x224ADE80),
    warn = Color(0xFFFBBF24),
    danger = Color(0xFFF87171),
)

private val LocalPalette = staticCompositionLocalOf { Light }

val pal: Palette
    @Composable @ReadOnlyComposable get() = LocalPalette.current

@Composable
fun AeriTheme(content: @Composable () -> Unit) {
    val p = if (isSystemInDarkTheme()) Dark else Light
    val scheme = if (p.dark) {
        darkColorScheme(
            primary = p.signal, onPrimary = Color.White, background = p.canvas, onBackground = p.ink,
            surface = p.surface, onSurface = p.ink, surfaceContainer = p.surface, inverseSurface = p.ink,
            inverseOnSurface = p.canvas, error = p.danger,
        )
    } else {
        lightColorScheme(
            primary = p.signal, onPrimary = Color.White, background = p.canvas, onBackground = p.ink,
            surface = p.surface, onSurface = p.ink, surfaceContainer = p.surface, inverseSurface = p.ink,
            inverseOnSurface = p.canvas, error = p.danger,
        )
    }
    CompositionLocalProvider(LocalPalette provides p) {
        MaterialTheme(colorScheme = scheme, content = content)
    }
}

/** 线条图标（24×24 描边，和电脑版一致） */
object AeriIcons {
    private const val SPEAKER = "M4 9.5v5h3.5L12 18.5v-13L7.5 9.5H4z"

    val mic = stroke("M12 3a3 3 0 0 0-3 3v6a3 3 0 0 0 6 0V6a3 3 0 0 0-3-3z M19 11a7 7 0 0 1-14 0 M12 18v3")
    val micOff = stroke("M12 3a3 3 0 0 0-3 3v6a3 3 0 0 0 6 0V6a3 3 0 0 0-3-3z M19 11a7 7 0 0 1-14 0 M12 18v3 M4 4l16 16")
    val keyboard = stroke("M4 6h16a1 1 0 0 1 1 1v10a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1V7a1 1 0 0 1 1-1z M7 10h1 M11 10h1 M15 10h2 M8 14h8")
    val send = stroke("M12 19V5 M6 11l6-6 6 6", 2.2f)
    val stop = stroke("M7.5 7.5h9v9h-9z", 2f)
    val volumeDown = stroke("$SPEAKER M16 9.5a3.5 3.5 0 0 1 0 5")
    val volumeUp = stroke("$SPEAKER M16 9.5a3.5 3.5 0 0 1 0 5 M18.5 7a7 7 0 0 1 0 10")
    val mute = stroke("$SPEAKER M16 10l4 4 M20 10l-4 4")
    val previous = stroke("M6 5v14 M18 5.5v13L8.5 12z")
    val next = stroke("M18 5v14 M6 5.5v13L15.5 12z")
    val playPause = stroke("M4 6v12l7.5-6z M15 6v12 M19.5 6v12")
    val refresh = stroke("M20 11a8 8 0 0 0-14.6-4.5 M4 4v4h4 M4 13a8 8 0 0 0 14.6 4.5 M20 20v-4h-4")
    val desktop = stroke("M4 5h16a1 1 0 0 1 1 1v9a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1V6a1 1 0 0 1 1-1z M9 20h6 M12 16v4")
    val more = fill(
        "M12 7a1.6 1.6 0 1 1 0-3.2 1.6 1.6 0 0 1 0 3.2z " +
            "M12 13.6a1.6 1.6 0 1 1 0-3.2 1.6 1.6 0 0 1 0 3.2z " +
            "M12 20.2a1.6 1.6 0 1 1 0-3.2 1.6 1.6 0 0 1 0 3.2z",
    )

    private fun builder() = ImageVector.Builder(
        defaultWidth = 24.dp, defaultHeight = 24.dp, viewportWidth = 24f, viewportHeight = 24f,
    )

    private fun stroke(path: String, width: Float = 1.8f) = builder().addPath(
        pathData = addPathNodes(path),
        fill = null,
        stroke = SolidColor(Color.Black),
        strokeLineWidth = width,
        strokeLineCap = StrokeCap.Round,
        strokeLineJoin = StrokeJoin.Round,
    ).build()

    private fun fill(path: String) = builder().addPath(pathData = addPathNodes(path), fill = SolidColor(Color.Black)).build()
}
