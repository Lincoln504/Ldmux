package io.ldmux.mobile

import androidx.compose.ui.text.font.Font
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight

/** Fira Mono Regular and Bold, the two cuts the upstream repository ships. */
actual val MonoFont: FontFamily = FontFamily(
    Font(R.font.fira_mono_regular, FontWeight.Normal),
    Font(R.font.fira_mono_bold, FontWeight.Bold),
)
