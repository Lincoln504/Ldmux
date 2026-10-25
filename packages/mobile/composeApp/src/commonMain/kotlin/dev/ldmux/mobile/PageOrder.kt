package dev.ldmux.mobile

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
 * An id the list does not carry *right now* keeps its place rather than being
 * pulled out and appended later: a session list that is momentarily stale (a
 * reconnect, a broadcast in flight) is not the user closing a shell, and a page that
 * jumps two places because the list blinked is the same class of fault.
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
fun addToOrder(order: List<String>, ids: List<String>): List<String> {
    val out = order.toMutableList()
    for (id in ids) if (id !in out) out.add(id)
    return out
}
