plugins {
    id("com.android.library")
}

android {
    namespace = "io.ldmux.mobile.ble"

    compileSdk = 37
    defaultConfig {
        minSdk = 28
        consumerProguardFiles("consumer-rules.pro")
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    buildTypes {
        release {
            isMinifyEnabled = false
        }
    }
    testOptions {
        unitTests.isReturnDefaultValues = true
    }
}

dependencies {
    // **The protocol is not here any more.** This module is the Android BLE transport and the two
    // types it needs (`BleScan`'s UUIDs and the `BleTransport` interface); the companion
    // protocol, its frames and its session live in Rust behind `ldmux_client_*`
    // (`docs/RADIO-PLAN.md` §8.1, entry `A2`).
    //
    // The Java sources use androidx.annotation.NonNull/Nullable.
    api("androidx.annotation:annotation:1.9.1")

    // Nordic BLE: connection management, MTU negotiation, serialized writes.
    api("no.nordicsemi.android:ble:2.10.1")

}
