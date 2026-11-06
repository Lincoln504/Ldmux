package io.ldmux.mobile.mesh

import io.ldmux.mobile.AndroidApp

actual fun meshSessionKeyStore(): MeshSessionKeyStore = KeystoreMeshSessionKeyStore(AndroidApp.context)
