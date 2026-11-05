package io.ldmux.mobile.mesh

import io.ldmux.mobile.AndroidApp

actual val currentPlatform: MobilePlatform = MobilePlatform.Android

actual fun meshSessionKeyStore(): MeshSessionKeyStore = KeystoreMeshSessionKeyStore(AndroidApp.context)
