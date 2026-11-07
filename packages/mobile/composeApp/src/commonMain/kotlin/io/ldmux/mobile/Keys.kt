package io.ldmux.mobile

/**
 * A control key as a person writes it: `C/c`, never `C-c`.
 *
 * `C-c` is the protocol's name for the key and is what has to go on the wire, so the
 * label is derived from the name rather than typed out next to it. Two bars show
 * these keys and only one of them used to be right, which is what typing a label
 * beside a key gets you.
 */
fun controlLabel(key: String): String =
    if (key.startsWith("C-")) "C/" + key.removePrefix("C-") else key

/**
 * The portrait bar's control row: label to action, in order.
 *
 * **A function rather than a list written at the call site, because a regression
 * guard can only guard what it can see.** This row and the panel's were inline
 * literals in `App.kt`, and the test that checks `C/d` stays gone read a separate
 * constant (the old `SIDEBAR_KEYS`) that the UI had stopped using — so adding `C/d`
 * to either bar left the guard green. It had two references in the whole repository:
 * its own definition and the test. A guard on dead code reads as protection and is
 * not, which is worse than no guard.
 *
 * Now there is one list, the UI renders it, and the test can call this with stubs and
 * read the labels it produced.
 */
fun portraitControlKeys(
    onSticky: (StickyKey) -> Unit,
    press: (String) -> Unit,
): List<Pair<String, () -> Unit>> = listOf(
    // The same latch as the panel's, from the same state: arm Ctrl, then press any
    // key, and Ctrl applies to that key. It used to be wired straight to `C-c` here,
    // which sent an interrupt instead of arming anything — so Ctrl-then-`d` sent C-c
    // and then a d.
    "Ctrl" to { onSticky(StickyKey.Ctrl) },
    "Alt" to { onSticky(StickyKey.Alt) },
    // Shift is a latch for the same reason as the other two: `Shift+Enter` is a
    // newline in a TUI (`pi` binds it), and a soft keyboard cannot hold Shift
    // while the bar's Enter is pressed. It also gives Shift+Tab and shift-arrows.
    "\u21e7" to { onSticky(StickyKey.Shift) },
    controlLabel("C-c") to { press("C-c") },
    // `|`, `-` and `/` were here and are gone: the soft keyboard produces all
    // three with one press, and the width they took is the arrow block's and the
    // paste key's. Landscape keeps them, because landscape has no soft keyboard.
)

/**
 * The landscape panel's control row: label to action, in order.
 *
 * The five things a soft keyboard cannot produce and a shell wants most. Ctrl-D is
 * deliberately not among them: it was, and it was asked for by name to be taken out.
 * It sends end-of-file, which closes the pane, and a key that ends a session has no
 * business on a bar someone presses without looking while holding the phone one-handed.
 */
fun panelControlKeys(press: (String) -> Unit): List<Pair<String, () -> Unit>> = listOf(
    controlLabel("C-c") to { press("C-c") },
    "|" to { press("|") },
    "-" to { press("-") },
    "/" to { press("/") },
)

/**
 * The six navigation keys as a **position**, not a list: two rows of three equal keys.
 *
 *     PgUp   Up   PgDn
 *     Left  Down  Right
 *
 * `Up` sits directly over `Down` and `Left`/`Right` either side of it, so the block
 * reads as a direction rather than four words in a row. Every cell holds one key of
 * one width, which is the part that took three attempts:
 *
 * The first version was `null, Up, null` over `Left, Down, Right`: an *empty cell*
 * above each of `Left` and `Right`. It drew the same picture and left two holes in
 * the bar, in the one place a thumb goes without looking (reported on the phone,
 * 2026-10-25: "blank button spaces above the left and right arrows"). The list has
 * no `null` in it any more, so the layout cannot grow a blank back.
 *
 * The second filled those holes with `Up` itself, one key spanning the whole row.
 * Drawn at three times the width of `Down`, `Left` and `Right`, the most-used key in
 * a terminal looked like a strip and not a direction, and the block read as two
 * rows of different kinds (reported on the phone, 2026-11-01: "the up arrow is extra
 * long; it should be the same width").
 *
 * So the two cells beside `Up` are the page keys, and they are the right two: a phone
 * terminal scrolls history far more often than it moves a cursor, both were already
 * in the bar's top row at a sixth of its width each, and page-up over page-down beside
 * the arrows is the cluster every keyboard puts together. The top row keeps `Esc`,
 * `Tab`, `Home` and `End`, which is the rest of that family, and it is now four keys
 * wide instead of six.
 *
 * It lives here, as data, so a layout change and the test that guards it read the
 * same thing. The bar rendered `← ↑ ↓ →` inline for a long time and nothing noticed,
 * because there was nothing to notice: the shape was a call site, not a value.
 */
/*
 * The names are the *protocol's*, not the labels: `press` sends what it is given, and the bar
 * used to spell the label at the call site (`"PgUp" to { press("PageUp") }`). A pad that
 * carried the label would send a key called `PgUp`, which the host has never heard of - so the
 * label is drawn by `glyphFor` and the name is what travels.
 */
val ARROW_PAD_ROWS: List<List<String>> = listOf(
    listOf("PageUp", "Up", "PageDown"),
    listOf("Left", "Down", "Right"),
)
