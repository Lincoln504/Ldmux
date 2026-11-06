package io.ldmux.mobile

/**
 * What a settings footer needs, and nothing more.
 *
 * **The version is the important part.** Every report that starts "the app is broken"
 * needs the build it was broken in, and a user should not have to find it in Android's own
 * app screen to say so. The site is the second line: it is where the project lives, and it
 * is the one link in the app that is not a machine's address.
 *
 * There is deliberately no copyright line. A claim about ownership is not something a
 * settings screen can make useful to a reader, and the licence files are the place that
 * question actually gets answered.
 */
expect fun appVersionName(): String

/** Open [url] in whatever the device uses for links. False when nothing could. */
expect fun openUrl(url: String): Boolean
