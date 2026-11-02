package dev.ldmux.mobile

import androidx.compose.material3.MaterialTheme
import dev.ldmux.mobile.mesh.MeshRouteActions
import dev.ldmux.mobile.mesh.applyMeshPairingScan
import dev.ldmux.mobile.mesh.availableRoutes
import dev.ldmux.mobile.mesh.currentPlatform
import dev.ldmux.mobile.mesh.MeshEnrollment
import dev.ldmux.mobile.mesh.MeshRoute
import dev.ldmux.mobile.mesh.MeshTerminalScreen
import dev.ldmux.mobile.mesh.meshScreenFor
import dev.ldmux.mobile.mesh.readMeshCredentials
import dev.ldmux.mobile.mesh.libraryKeypair
import dev.ldmux.mobile.mesh.meshSessionKeyStore
import androidx.compose.material3.DropdownMenu
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.material3.LocalTextStyle

/**
 * The app, inside one theme.
 *
 * It had no root theme at all. `MaterialTheme` appeared exactly once in this whole
 * package, wrapped around the overflow menu's `DropdownMenu` — so that menu was the
 * only place in the app where a Material default applied, and every other control
 * took `LocalIndication` (and every other Material composition local) from a platform
 * fallback that nothing here chose and nothing here could see. That is the real reason
 * one control felt responsive and the next did not: it was not two treatments, it was
 * one control with Material's ripple and the next with none of Material's anything.
 *
 * It is the same scheme the menu already used, so the menu is unchanged. What changes
 * is that everything is now inside it.
 */
@Composable
fun App() {
    // The config lives above the theme and above the touch scale, because it decides
    // both. A setting that reached only some of the controls would be worse than no
    // setting: the whole reason this one exists is that the app disagreed with
    // itself about the same control in two places.
    val store = remember { ConfigStore(AppPrefs.load(), MeshEnrollment(meshSessionKeyStore(), ::libraryKeypair), AppPrefs::save) }
    val config by store.config.collectAsState()
    MaterialTheme(colorScheme = LdmuxColors) {
        CompositionLocalProvider(
            LocalTextStyle provides BodyStyle,
            LocalTouch provides touchFor(config.compact),
        ) {
            // The mesh route replaces the whole terminal surface, not a part of it: it is a different,
            // reduced terminal (ADR-0025). Anything that cannot run is left to AppContent, which
            // shows why and opens nothing. The tailnet path below is exactly what it was.
            // Leaving the mesh screen is a **view** change, not a configuration change. It used
            // to rewrite the saved route to tailnet, so a person who looked away once came back to
            // The route is chosen in the app's own header (a dropdown), so there is no
            // separate exit from the mesh route and no separate screen to come back from.
            val routes = availableRoutes(currentPlatform).map {
                RouteChoice(if (it == MeshRoute.Mesh) "mesh" else "tailnet", it == MeshRoute.Mesh)
            }
            val current = RouteChoice(if (config.route == MeshRoute.Mesh) "mesh" else "tailnet", config.route == MeshRoute.Mesh)
            val choose: (RouteChoice) -> Unit = { c ->
                store.update(config.copy(route = if (c.mesh) MeshRoute.Mesh else MeshRoute.Tailnet))
            }
            // **Built once per configuration, not once per recomposition.** `MeshScreenPlan` has no
            // equality, and the mesh screen keys its whole connection on it, so every recomposition of
            // this scope (opening Settings is one) made a new plan and with it a new connection: a
            // fresh handshake, which is minutes on the radio. The credentials version is the other
            // way in: scanning or fetching a pairing changes secure storage and not the config.
            // `connectionKey` is the radio/tailnet identity without font, theme or other view state.
            var credentialsVersion by remember { mutableStateOf(0) }
            val mesh = remember(config.connectionKey, credentialsVersion) { meshScreenFor(config) }
            // Settings is drawn **once, here**, for every route. It used to belong to the
            // tailnet surface, so the mesh terminal had no way in at all: you had to leave
            // the mesh route to reach Settings, and leaving then reported the route as
            // unavailable. One app, one Settings.
            var showSettings by remember { mutableStateOf(false) }
            if (mesh != null) {
                MeshTerminalScreen(mesh, config, onOpenSettings = { showSettings = true }, routes = routes, currentRoute = current, onRoute = choose)
            } else {
                AppContent(config, store, onOpenSettings = { showSettings = true }, routes = routes, currentRoute = current, onRoute = choose)
            }
            if (showSettings) {
                SettingsDialog(
                    current = config,
                    onDismiss = { showSettings = false },
                    onSave = {
                        store.update(it)
                        showSettings = false
                    },
                    onPaired = { p ->
                        store.pair(p)
                        showSettings = false
                    },
                    onUnpair = { store.unpair() },
                    mesh = if (MeshRoute.Mesh in availableRoutes(currentPlatform)) {
                        MeshRouteActions(
                            credentials = { readMeshCredentials(meshSessionKeyStore()).first },
                            onPairing = { text -> applyMeshPairingScan(text, store).also { credentialsVersion++ } },
                            onConfig = { store.update(it) },
                            onUnpair = { store.unpairMesh(); credentialsVersion++ },
                        )
                    } else null,
                )
            }
        }
    }
}
