package dev.ldmux.mobile.voice

/** What to hand a speech engine, and whether the answer was cut short. */
data class SpeechChunks(val chunks: List<String>, val truncated: Boolean)

/** One utterance at a time is easier to interrupt and to queue than a paragraph. */
const val SPEECH_CHUNK_MAX_CHARS: Int = 240

/** A runaway answer must not monologue: past this many characters the rest is not spoken. */
const val SPEECH_TOTAL_MAX_CHARS: Int = 1200

private val SENTENCE_END = charArrayOf('.', '!', '?', '\u3002', '\uFF01', '\uFF1F', '\n')

/**
 * Split text into sentence-sized pieces for a TTS queue.
 *
 * Boundaries are sentence punctuation (Latin and CJK) and newlines; a piece longer than
 * [maxChunk] is broken at the last space before the limit, or hard at the limit when there is
 * none (an unbroken CJK run). Whitespace-only pieces are dropped. Total spoken length is capped
 * at [maxTotal], cut at a piece boundary, and [SpeechChunks.truncated] says so, so the UI can
 * say it rather than let an answer silently end mid-thought.
 */
fun chunkForSpeech(
    text: String,
    maxChunk: Int = SPEECH_CHUNK_MAX_CHARS,
    maxTotal: Int = SPEECH_TOTAL_MAX_CHARS,
): SpeechChunks {
    val sentences = ArrayList<String>()
    val cur = StringBuilder()
    for (ch in text) {
        cur.append(ch)
        if (ch in SENTENCE_END) {
            sentences += cur.toString()
            cur.setLength(0)
        }
    }
    if (cur.isNotEmpty()) sentences += cur.toString()

    val pieces = ArrayList<String>()
    for (raw in sentences) {
        var s = raw.trim()
        while (s.length > maxChunk) {
            var cut = s.lastIndexOf(' ', maxChunk)
            if (cut <= 0) cut = maxChunk
            pieces += s.substring(0, cut).trim()
            s = s.substring(cut).trim()
        }
        if (s.isNotEmpty()) pieces += s
    }
    // Small neighbours share an utterance: a queue of one-word chunks sounds like stuttering.
    val packed = ArrayList<String>()
    for (p in pieces.filter { it.isNotEmpty() }) {
        val last = packed.lastOrNull()
        if (last != null && last.length + 1 + p.length <= maxChunk) {
            packed[packed.lastIndex] = "$last $p"
        } else {
            packed += p
        }
    }
    var total = 0
    val out = ArrayList<String>()
    var truncated = false
    for (p in packed) {
        if (total + p.length > maxTotal) {
            truncated = true
            break
        }
        out += p
        total += p.length
    }
    // A first chunk that alone exceeds the cap cannot happen (maxChunk <= maxTotal), but a
    // caller passing odd limits must still get a refusal rather than silence with no flag.
    if (out.isEmpty() && packed.isNotEmpty()) truncated = true
    return SpeechChunks(out, truncated)
}
