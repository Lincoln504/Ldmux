package io.ldmux.mobile

/**
 * What a settings footer needs, and nothing more.
 *
 * **The version is the important part.** Every report that starts "the app is broken"
 * needs the build it was broken in, and a user should not have to find it in Android's own
 * app screen to say so.
 *
 * There is no site link. `openUrl` and the footer line it was written for were removed
 * (2026-11-07): nothing in the app reached either, and a link to a project page is not
 * something a terminal needs from the screen that reports its build.
 *
 * There is deliberately no copyright line. A claim about ownership is not something a
 * settings screen can make useful to a reader, and the licence files are the place that
 * question actually gets answered.
 */
expect fun appVersionName(): String
