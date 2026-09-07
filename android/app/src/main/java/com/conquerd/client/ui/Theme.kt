package com.conquerd.client.ui

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

// The desktop client's accent, so the two clients read as one product.
private val ConquerdAccent = Color(0xFF3B82F6)
private val ConquerdAccentDark = Color(0xFF60A5FA)

private val LightColors = lightColorScheme(
    primary = ConquerdAccent,
    secondary = Color(0xFF64748B),
)

private val DarkColors = darkColorScheme(
    primary = ConquerdAccentDark,
    secondary = Color(0xFF94A3B8),
)

@Composable
fun ConquerdTheme(
    darkTheme: Boolean = isSystemInDarkTheme(),
    content: @Composable () -> Unit,
) {
    // Material You where the platform offers it: on a Pixel the wallpaper
    // palette is what every other app uses, and overriding it looks foreign.
    val colors = when {
        Build.VERSION.SDK_INT >= Build.VERSION_CODES.S -> {
            val context = LocalContext.current
            if (darkTheme) dynamicDarkColorScheme(context) else dynamicLightColorScheme(context)
        }

        darkTheme -> DarkColors
        else -> LightColors
    }

    MaterialTheme(colorScheme = colors, content = content)
}
