package dev.ldmux.mobile.voice

import android.Manifest
import android.content.pm.PackageManager
import android.media.AudioFormat
import android.media.AudioRecord
import android.media.MediaRecorder
import android.os.Handler
import android.os.Looper

private const val SAMPLE_RATE = 16_000
private const val FRAME_SAMPLES = SAMPLE_RATE / 50 // 20 ms

/**
 * PCM for barge-in: [AudioRecord] on the voice-communication source (which is the one the
 * platform runs echo cancellation on, so the phone's own speaker is attenuated), 16 kHz mono,
 * 20 ms frames. It decides nothing: frames go to common code ([SpeechDetector]), posted to the
 * main thread because the state machine behind them is not thread-safe.
 *
 * It does not ask for the microphone permission: [Dictation] owns that, and voice mode has
 * already been through it before anything is spoken. Without it, [start] reports an error.
 */
private class AudioRecordCapture : CapturePort {
    private val main = Handler(Looper.getMainLooper())
    @Volatile private var running = false
    private var thread: Thread? = null

    override fun start(onFrame: (ShortArray) -> Unit, onError: (String) -> Unit) {
        val activity = Dictation.hostActivity()
        if (activity == null || activity.checkSelfPermission(Manifest.permission.RECORD_AUDIO) != PackageManager.PERMISSION_GRANTED) {
            onError("no microphone permission")
            return
        }
        val minBuffer = AudioRecord.getMinBufferSize(SAMPLE_RATE, AudioFormat.CHANNEL_IN_MONO, AudioFormat.ENCODING_PCM_16BIT)
        if (minBuffer <= 0) {
            onError("no audio input on this device")
            return
        }
        val record = try {
            @Suppress("MissingPermission") // checked just above
            AudioRecord(
                MediaRecorder.AudioSource.VOICE_COMMUNICATION,
                SAMPLE_RATE,
                AudioFormat.CHANNEL_IN_MONO,
                AudioFormat.ENCODING_PCM_16BIT,
                maxOf(minBuffer, FRAME_SAMPLES * 4),
            )
        } catch (e: Exception) {
            onError(e.message ?: "could not open the microphone")
            return
        }
        if (record.state != AudioRecord.STATE_INITIALIZED) {
            record.release()
            onError("the microphone is in use")
            return
        }
        running = true
        thread = Thread({
            try {
                record.startRecording()
                val buf = ShortArray(FRAME_SAMPLES)
                while (running) {
                    var got = 0
                    while (got < buf.size && running) {
                        val n = record.read(buf, got, buf.size - got)
                        if (n <= 0) throw IllegalStateException("audio read failed ($n)")
                        got += n
                    }
                    if (running) {
                        val frame = buf.copyOf()
                        main.post { if (running) onFrame(frame) }
                    }
                }
            } catch (e: Exception) {
                if (running) {
                    running = false
                    main.post { onError(e.message ?: "audio capture stopped") }
                }
            } finally {
                runCatching { record.stop() }
                record.release()
            }
        }, "ldmux-voice-capture").also { it.isDaemon = true; it.start() }
    }

    override fun stop() {
        running = false
        thread = null
    }
}

actual fun newCapture(): CapturePort? = AudioRecordCapture()
