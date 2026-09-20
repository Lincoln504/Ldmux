package dev.ldmux.mobile

/**
 * A line of diagnostic output, on the platform's own log.
 *
 * This exists because there was no way to ask this app a question. `println` does
 * not reach logcat, commonMain cannot see `android.util.Log`, and the only surface
 * that rendered anything was the error banner — which clears itself after four
 * seconds, so a screenshot at the wrong moment said "no message arrived" when a
 * message had arrived and gone. Diagnosing the empty launcher took far longer than
 * it should have for want of one function.
 *
 * Deliberately a seam and not a framework: no levels, no tags-by-convention, no
 * configuration. A thing worth saying goes to the platform log under one tag.
 */
expect fun appLog(message: String)
