package dev.ldmux.mobile

import dev.ldmux.mobile.term.TerminalFrame

/**
 * A decoded frame for tests that care about a handful of its fields.
 *
 * One builder, so a change to [TerminalFrame.Frame]'s constructor is one edit here
 * rather than one per test file.
 */
internal fun testFrame(
    cols: Int = 80,
    rows: Int = 24,
    alternateOn: Boolean = false,
    viewportActive: Boolean = true,
    passwordInput: Boolean = false,
) = TerminalFrame.Frame(
    cols = cols,
    rows = rows,
    cursorX = 0,
    cursorY = 0,
    cursorVisible = true,
    cursorKnown = true,
    cursorStyle = 1,
    defaultFg = 0,
    defaultBg = 0,
    passwordInput = passwordInput,
    viewportActive = viewportActive,
    mouseTracking = false,
    mouseSgr = false,
    alternateOn = alternateOn,
    selection = null,
    scrollTotal = 0,
    scrollOffset = 0,
    scrollLen = 0,
    cells = emptyList(),
)

/** A paired config on a tailnet address, for the client tests. */
internal fun testConfig(host: String = "100.64.0.9:7625") =
    LdmuxConfig(host = host, token = "a".repeat(64), deviceId = "dev-1", deviceLabel = "Test phone")
