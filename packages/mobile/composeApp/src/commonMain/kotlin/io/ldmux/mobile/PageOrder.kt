package io.ldmux.mobile

/**
 * The pager's pages: the shells the user opened, **in the order they opened them**.
 *
 * This is one line of mapping and it is the fix for the worst bug the shell view had
 * (reported 2026-10-25): the pages were `attachable.filter { it.id in selection }`,
 * i.e. the daemon's session order — and the daemon sorts live sessions by *last
 * activity* (`sessions.sort_by` in `crates/daemon/src/daemon.rs`), so a shell that
 * printed anything moved to the front. With several shells open, the page at index N
 * became a different shell every time any of them produced output, and the pager
 * slid back and forth as the client tried to put the shell it was showing back at
 * the index it had been at. Typing in one shell was enough to make the screen look
 * like it was swiping between shells.
 *
 * An id the list does not carry *right now* keeps its place in the order rather
 * than being pulled out and appended later: a session list that is momentarily stale
 * (a reconnect, a broadcast in flight) is not the user closing a shell. It is omitted
 * from the pages while it is missing — so a page after it sits one earlier — and it
 * returns at its own index, which is what stops a list that blinks from permuting
 * the set the user is paging through.
 *
 * Duplicates are dropped: the same shell twice would be two pages on one pty, and
 * two claims on a shell is what ADR-0010 exists to prevent.
 */
fun pagesInOrder(order: List<String>, attachable: List<Session>): List<Session> {
    val byId = attachable.associateBy { it.id }
    val seen = HashSet<String>(order.size)
    return order.mapNotNull { id -> if (seen.add(id)) byId[id] else null }
}

/**
 * The one place a shell joins the page order: once, at the end, where it was opened.
 *
 * `+ shell` and the launcher both come through here so a page can never be inserted
 * in the middle of the set the user is paging through.
 */
fun addToOrder(
    order: List<String>,
    ids: List<String>,
    max: Int = MAX_OPEN_PAGES,
): List<String> {
    val out = order.toMutableList()
    for (id in ids) {
        if (out.size >= max) break
        if (id !in out) out.add(id)
    }
    return out
}

/**
 * How many shells the app keeps open at once.
 *
 * **A limit is a promise about state, not about the host.** Every open page holds a live stream, a
 * claim and a fitted grid, and the daemon serves as many as are asked for — so the client is the
 * only place a number belongs. Five is what a person can page between on a phone without losing
 * which one is which, and it is the same number on both routes: on the tailnet a page is a stream,
 * on the radio a page is a shell this phone has claimed.
 *
 * Reaching it is refused with a sentence rather than silently opening a sixth: a shell created and
 * not paged to is a shell the user cannot reach, which is worse than a refusal they can read.
 */
const val MAX_OPEN_PAGES = 5
