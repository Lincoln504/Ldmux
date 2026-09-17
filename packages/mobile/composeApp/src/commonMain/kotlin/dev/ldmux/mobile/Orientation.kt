package dev.ldmux.mobile

/**
 * Lock the whole screen to portrait or landscape.
 *
 * Android has **one** orientation per activity, so this is the whole screen and
 * not one view inside it: a phone app cannot show a portrait pane beside a
 * landscape one. (Jetpack WindowManager's activity embedding splits a task into
 * two containers, but both are multi-window, and multi-window activities have
 * their orientation requests ignored since Android 12 — so it cannot give two
 * orientations either.) Rotating the *content* 90° inside a portrait activity is
 * possible and is the only way to fake it, but it costs the system bars and the
 * soft keyboard: both follow the activity, so a keyboard would come up across a
 * sideways strip. See docs/adr/0011-screen-orientation.md.
 *
 * Locked rather than sensor-driven, and deliberately: the app decides what shape
 * it is, so a phone put down at an angle does not reflow a terminal with a
 * command half-typed in it.
 *
 * Called from exactly one place (see `App`): a second writer would be two
 * opinions about the same activity, and whichever ran last would win silently.
 */
expect fun lockOrientation(portrait: Boolean)
