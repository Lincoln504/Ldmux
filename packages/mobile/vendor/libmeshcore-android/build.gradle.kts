plugins {
    id("com.android.library")
}

android {
    namespace = "io.iotone.meshcore.android"

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
    // Protocol layer, built from the same source tree.
    api(project(":libmeshcore"))

    // The Java sources use androidx.annotation.NonNull/Nullable.
    api("androidx.annotation:annotation:1.9.1")

    // Nordic BLE: connection management, MTU negotiation, serialized writes.
    api("no.nordicsemi.android:ble:2.10.1")

    testImplementation("junit:junit:4.13.2")
}
