package io.ldmux.mobile.term

/**
 * How many terminal **cells** one code point occupies.
 *
 * A terminal's grid is counted in cells, and a character is not a cell: a CJK ideograph or an emoji
 * is two, a combining accent is none. Anything that cuts or counts text against a column count has
 * to count cells, and two places did not — the mesh route's row cleanup and its local echo both
 * counted UTF-16 code points, so a row of ideographs was cut in the wrong place and a row of
 * emoji could end up a cell wider than the grid it was drawn into.
 *
 * **This is the common subset of Unicode's East Asian Width table, not all of it**, and the two
 * directions are not equally costly. A character the table does not know counts as one cell. That
 * can make a decision one cell generous, which for the two callers is harmless: `cleanRow` is a
 * guard against a row longer than the grid (a well-formed row from the host is already exactly the
 * host's width, wide characters included), and the echo window re-derives its own width from the
 * cells it emitted. Counting a wide character as one, in the other direction, would cut a row
 * mid-glyph — the failure this replaces.
 *
 * Control characters are zero cells here because they occupy none on screen; every caller strips
 * or interprets them separately.
 */
internal fun displayWidthOf(codePoint: Int): Int = when {
    // C0, DEL, C1: not drawn at all. Callers filter them; counting them at zero keeps a miscount
    // from silently widening a row.
    codePoint < 0x20 || codePoint in 0x7f..0x9f -> 0
    // Non-spacing marks. Latin, Greek and Cyrillic diacritics are the overwhelming majority of what
    // a terminal sees; the Indic and Arabic blocks that are not listed fall through to one cell.
    codePoint in 0x0300..0x036f -> 0
    codePoint in 0x0483..0x0489 -> 0
    codePoint in 0x0591..0x05bd -> 0
    codePoint in 0x0610..0x061a -> 0
    codePoint in 0x064b..0x065f -> 0
    codePoint in 0x06d6..0x06ed -> 0
    // Zero-width space, the joiners, and the bidi marks: present, invisible, no cell.
    codePoint in 0x200b..0x200f -> 0
    codePoint in 0x2060..0x2064 -> 0
    // Variation selectors, which modify the glyph before them and never advance the cursor.
    codePoint in 0xfe00..0xfe0f -> 0
    codePoint == 0xfeff -> 0
    isWideCodePoint(codePoint) -> 2
    else -> 1
}

/** [text]'s width in cells, with surrogate pairs read as one code point each. */
internal fun displayWidthOf(text: String): Int {
    var cells = 0
    var i = 0
    while (i < text.length) {
        val cp = codePointAt(text, i)
        cells += displayWidthOf(cp)
        i += charCountOf(cp)
    }
    return cells
}

/**
 * The code point at [index] of [text], combining a surrogate pair when there is one.
 *
 * `Character.codePointAt` and `Character.charCount` are JVM-only and this code is shared, so the
 * two lines of them that matter live here, once, for every caller that walks a string by cell.
 */
internal fun codePointAt(text: String, index: Int): Int {
    val high = text[index]
    if (high.isHighSurrogate() && index + 1 < text.length) {
        val low = text[index + 1]
        if (low.isLowSurrogate()) {
            return ((high.code - 0xd800) shl 10) + (low.code - 0xdc00) + 0x10000
        }
    }
    return high.code
}

/** How many UTF-16 units [codePoint] occupies: two above the basic plane, one below it. */
internal fun charCountOf(codePoint: Int): Int = if (codePoint > 0xffff) 2 else 1

/** A character that takes two cells: CJK, Hangul, kana, fullwidth forms, and the emoji planes. */
private fun isWideCodePoint(codePoint: Int): Boolean = when (codePoint) {
    in 0x1100..0x115f -> true // Hangul Jamo, initial consonants
    in 0x2e80..0x303e -> true // CJK radicals and punctuation
    in 0x3041..0x33ff -> true // Hiragana, Katakana, Bopomofo, CJK compatibility
    in 0x3400..0x4dbf -> true // CJK unified ideographs extension A
    in 0x4e00..0x9fff -> true // CJK unified ideographs
    in 0xa000..0xa4cf -> true // Yi
    in 0xa960..0xa97f -> true // Hangul Jamo extended A
    in 0xac00..0xd7a3 -> true // Hangul syllables
    in 0xf900..0xfaff -> true // CJK compatibility ideographs
    in 0xfe10..0xfe19 -> true // Vertical forms
    in 0xfe30..0xfe6f -> true // CJK compatibility forms
    in 0xff00..0xff60 -> true // Fullwidth forms
    in 0xffe0..0xffe6 -> true // Fullwidth signs
    in 0x1f000..0x1f02f -> true // Mahjong tiles and dominoes
    in 0x1f300..0x1f64f -> true // Emoji: symbols, pictographs, emoticons
    in 0x1f900..0x1f9ff -> true // Supplemental symbols and pictographs
    in 0x20000..0x3fffd -> true // CJK extension B and beyond
    else -> false
}
