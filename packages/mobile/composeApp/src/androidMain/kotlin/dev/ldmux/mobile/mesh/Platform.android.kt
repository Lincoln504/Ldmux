package dev.ldmux.mobile.mesh

import dev.ldmux.mobile.AndroidApp

actual val currentPlatform: MobilePlatform = MobilePlatform.Android

actual fun meshSessionKeyStore(): MeshSessionKeyStore = KeystoreMeshSessionKeyStore(AndroidApp.context)
