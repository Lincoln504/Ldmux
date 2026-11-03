package dev.ldmux.mobile

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.pager.HorizontalPager
import androidx.compose.foundation.pager.PagerState
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.DisposableEffect
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/** Rows one tap of the scroll pad moves. A nudge, not a page: a page is a swipe. */
internal const val SCROLL_STEP = 3

/**
 * The terminal itself, laid out the same in both modes. Handles are registered
 * here so the chrome (whichever chrome is showing) can drive the visible one.
 */
@Composable
internal fun TerminalArea(
    config: LdmuxConfig,
    pages: List<Session>,
    pagerState: PagerState,
    handles: MutableMap<String, TerminalHandle>,
    typing: Boolean,
    // Which shell is on screen, by id. The pager's page index is reset
    // underneath the user by a re-measure (see `shownSessionId` in App), and
    // "connected" decided from that index would attach — and take over, which
    // a first attach is (ADR-0010) — a shell nobody is looking at for as long
    // as the reset lasts.
    shownId: String?,
    modifier: Modifier,
    keyboardRequest: Int = 0,
    // A shell just asked for has not arrived yet (see `pendingOpens` in App()) —
    // that is not "no sessions", it is "one is on its way", and saying the wrong
    // one of those is exactly what read as a bug: the launcher had three things
    // on it, one was tapped, and the terminal screen it landed on flatly denied
    // anything was open.
    pendingOpen: Boolean = false,
    /**
     * A page's shell is gone: ended, or unknown to the daemon.
     *
     * The caller drops the page, which is what it also does for a shell that left
     * the session list. Passed in rather than handled here because the page set and
     * the launcher's visibility live with the caller.
     */
    onShellGone: (String) -> Unit = {},
) {
    Box(modifier.background(Bg)) {
        if (pages.isEmpty()) {
            Text(
                if (pendingOpen) "Opening a shell…" else "No sessions yet. Tap + shell to start one.",
                color = Dim,
                fontSize = 13.sp,
                modifier = Modifier.align(Alignment.Center).padding(16.dp),
            )
        } else {
            // **The page is keyed by the shell, not by the slot.** Without a key the
            // pager reuses the composition at an index, so when the session list
            // reorders (a shell dies, the daemon broadcasts a new list) the page at
            // index N becomes a *different* shell with the *same* composed
            // `pointerInput` blocks. Those blocks are keyed on `Unit` precisely so a
            // gesture is not restarted mid-drag, so they kept the controller they
            // were born with — the old shell's, already disposed by the holder that
            // replaced it. Scroll, tap and selection then went to nothing, and only
            // looked correct again after a rotation rebuilt the page. A key makes a
            // different shell a different composition, and the gestures are born
            // with the controller they act on.
            HorizontalPager(
                state = pagerState,
                beyondViewportPageCount = 2,
                key = { page -> pages.getOrNull(page)?.id ?: page },
                modifier = Modifier.fillMaxSize(),
            ) { page ->
              // One Box per page, and it is not cosmetic: a pager page stacks its
              // children only if the content is itself a Box. As two siblings the
              // overlay was laid out *after* a fillMaxSize terminal, i.e. below the
              // bottom of the screen — so a refused attach showed a blank terminal
              // and no explanation, which is exactly what it was built to prevent.
              Box(Modifier.fillMaxSize()) {
              // A page index and a list that can change size are two independent
              // things, and the daemon broadcasts the session list on every change:
              // a shell dying can shrink `pages` between the pager deciding to
              // compose a page and this line being read, which is an index out of
              // bounds and a crash on a screen the user did nothing wrong on. A
              // shell going away is a page that stops being drawn.
                val s = pages.getOrNull(page) ?: return@HorizontalPager
                // Composed if it is within a swipe, connected only if it is the
                // one on screen *and the app is on screen* (see
                // TerminalHandle.rememberTerminalSurface). An app in the
                // background is not a reader, and holding the claim while nothing
                // is looking at it is what left a desk window on "Press r to take
                // it back" with no shell open anywhere (2026-10-05).
                val handle = rememberTerminalSurface(
                    config,
                    s.id,
                    typing,
                    active = pageIsConnected(s.id == shownId, AppLifecycle.foreground),
                    Modifier.fillMaxSize(),
                    keyboardRequest = keyboardRequest,
                )
                DisposableEffect(handle, s.id) {
                    handles[s.id] = handle
                    // A page that leaves composition is disposed, and its handle must
                    // leave with it. This map was written and read and never pruned,
                    // so it grew one disposed controller per session shown — each
                    // holding a frame grid — and a read in the frame before a page
                    // re-registered got the *disposed* one. That is survivable only
                    // because every engine call is guarded on a non-zero handle, so
                    // the symptom is a key that does nothing rather than a fault.
                    // Only remove our own entry: a re-registration must not have its
                    // handle dropped by the outgoing page's teardown.
                    onDispose { if (handles[s.id] === handle) handles.remove(s.id) }
                }
                // Who has this shell, when it is not us (ADR-0010). Drawn over
                // the terminal rather than replacing the page: the page is still
                // real, it is just somebody else's to type in right now — and a
                // blank terminal with no reason on it is the failure this screen
                // exists to prevent.
                val madeVisitable by handle.ownedBy.collectAsState()
                madeVisitable?.let { holder ->
                    HeldElsewhere(holder = holder, onTakeOver = { handle.takeOver() })
                }
                // The shell is gone: ended, or unknown to the daemon. Drop the page
                // on the attach's own verdict rather than waiting for the session
                // list to stop carrying it — the list arrives when the list socket
                // is up, and a page left behind is a frozen frame with nothing to
                // read and no retry.
                val gone by handle.ended.collectAsState()
                LaunchedEffect(gone) {
                    if (gone) onShellGone(s.id)
                }
                // Say so when input has nowhere to go. It is refused (not queued, not
                // painted by prediction) while the link is down, and a refusal with no
                // message is a terminal that has silently stopped listening.
                // The scroll controls, always on the terminal rather than only while it is
                // scrolled: a drag is the reading gesture, and these are the two destinations a
                // terminal needs without one.
                ScrollPad(
                    onUp = { handle.scrollBy(-SCROLL_STEP) },
                    onDown = { handle.scrollBy(SCROLL_STEP) },
                    onBottom = { handle.scrollToBottom() },
                    modifier = Modifier.align(Alignment.TopEnd),
                )
                val linkNow by handle.link.collectAsState()
                val tokenRefused by handle.rejected.collectAsState()
                val inputRefused by handle.inputRefused.collectAsState()
                if (tokenRefused) {
                    LinkNotice("pairing rejected \u00b7 pair again in settings")
                } else if (linkNow.offline && madeVisitable == null) {
                    LinkNotice("offline \u00b7 input is not being sent")
                } else if (inputRefused != null) {
                    // Said instead of nothing: the key was taken by this page and then dropped by
                    // the pane, which is the one case where the screen looks like the typing worked.
                    LinkNotice(inputRefused!!)
                }
              }
            }
        }
    }
}
