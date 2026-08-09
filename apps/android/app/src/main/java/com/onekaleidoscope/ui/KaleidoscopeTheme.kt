package com.onekaleidoscope.ui

import android.app.Activity
import android.os.Build
import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.dynamicDarkColorScheme
import androidx.compose.material3.dynamicLightColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalContext

private val LightColors = lightColorScheme(
    primary = Color(0xFF315AA8),
    onPrimary = Color.White,
    primaryContainer = Color(0xFFD9E2FF),
    onPrimaryContainer = Color(0xFF001A42),
    secondary = Color(0xFF555F71),
    secondaryContainer = Color(0xFFD9E3F8),
    tertiary = Color(0xFF6D5677),
    error = Color(0xFFBA1A1A),
    background = Color(0xFFF9F9FC),
    surface = Color(0xFFF9F9FC),
    surfaceVariant = Color(0xFFE1E2E8),
)

private val DarkColors = darkColorScheme(
    primary = Color(0xFFAFC6FF),
    onPrimary = Color(0xFF002E6A),
    primaryContainer = Color(0xFF164388),
    onPrimaryContainer = Color(0xFFD9E2FF),
    secondary = Color(0xFFBDC7DC),
    secondaryContainer = Color(0xFF3D4758),
    tertiary = Color(0xFFD9BDE3),
    error = Color(0xFFFFB4AB),
    background = Color(0xFF111318),
    surface = Color(0xFF111318),
    surfaceVariant = Color(0xFF44474F),
)

@Composable
fun KaleidoscopeTheme(
    darkTheme: Boolean = isSystemInDarkTheme(),
    dynamicColor: Boolean = true,
    content: @Composable () -> Unit,
) {
    val context = LocalContext.current
    val colors = when {
        dynamicColor && Build.VERSION.SDK_INT >= Build.VERSION_CODES.S -> {
            if (darkTheme) dynamicDarkColorScheme(context) else dynamicLightColorScheme(context)
        }
        darkTheme -> DarkColors
        else -> LightColors
    }

    val activity = context as? Activity
    activity?.window?.decorView?.setBackgroundColor(colors.background.toArgb())
    MaterialTheme(colorScheme = colors, content = content)
}

private fun Color.toArgb(): Int = android.graphics.Color.argb(
    (alpha * 255).toInt(),
    (red * 255).toInt(),
    (green * 255).toInt(),
    (blue * 255).toInt(),
)
