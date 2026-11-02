import org.jetbrains.kotlin.gradle.dsl.JvmTarget

plugins {
    alias(libs.plugins.kotlinMultiplatform)
    alias(libs.plugins.kotlinSerialization)
    alias(libs.plugins.androidApplication)
    alias(libs.plugins.composeMultiplatform)
    alias(libs.plugins.composeCompiler)
}

kotlin {
    androidTarget {
        compilerOptions {
            jvmTarget.set(JvmTarget.JVM_17)
        }
    }

    sourceSets {
        commonMain.dependencies {
            implementation(compose.runtime)
            implementation(compose.foundation)
            implementation(compose.material3)
            implementation(compose.ui)
            implementation(libs.kotlinx.coroutines.core)
            implementation(libs.kotlinx.serialization.json)
            implementation(libs.ktor.client.core)
            implementation(libs.ktor.client.websockets)
        }
        androidMain.dependencies {
            implementation(libs.androidx.activity.compose)
            implementation(libs.kotlinx.coroutines.android)
            implementation(libs.ktor.client.okhttp)
            // The pairing scanner (ADR-0017): no camera permission, Play services' own UI.
            implementation(libs.play.services.code.scanner)
            implementation(project(":libmeshcore-android"))
        }
        // The frame decoder is pure Kotlin on purpose: the wire format between
        // the JNI shim and the renderer is the one thing that silently rots when
        // the C side changes, so it is pinned by a test that needs no device.
        androidUnitTest.dependencies {
            implementation(kotlin("test"))
        }
    }
}

android {
    namespace = "dev.ldmux.mobile"
    // Compile against the latest released platform (37.2), but TARGET 36 and
    // MIN 28: Android 8.0 reached end of security support in January 2021, so a
    // phone on 26 or 27 has had no security updates for five years and cannot
    // run a terminal safely whatever it is a terminal to. Android 9 (API 28) is
    // still the ~95th percentile of devices in use.
    // MIN 28, and 26 was considered and rejected — the reasoning above is the whole
    // of it. (This block used to argue for MIN 26, three lines above the `minSdk = 28`
    // below it, so the comment nearest the value contradicted it.)
    compileSdk = 37
    compileSdkMinor = 2

    defaultConfig {
        applicationId = "dev.ldmux.mobile"
        minSdk = 28
        targetSdk = 36
        versionCode = 1
        versionName = "0.1.0"
    }

    packaging {
        resources.excludes += "/META-INF/{AL2.0,LGPL2.1}"
    }

    buildTypes {
        getByName("release") {
            isMinifyEnabled = false
        }
    }

    testOptions {
        // JVM unit tests run against the SDK's stub jar. Without this `appLog` (and any
        // other android.* call on a path under test) throws "not mocked"; with it the
        // stubs return defaults, so the *shared* logic is testable without a device.
        unitTests.isReturnDefaultValues = true
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}

// The L3 session is Rust behind a C ABI (native/ldmux_session.h). The JVM unit tests load the
// real library, so build its host variant first and point java.library.path at it. A failure
// here is not fatal to the build: MeshSessionAbiTest fails loudly with the reason, and every
// test that does not need the library still runs.
val sessionHostDir = rootProject.layout.projectDirectory.dir("native/build/host").asFile
val buildSessionHost = tasks.register<Exec>("buildSessionHost") {
    workingDir = rootProject.layout.projectDirectory.dir("native").asFile
    environment("JAVA_HOME", System.getProperty("java.home"))
    commandLine("./build-session.sh", "host")
    isIgnoreExitValue = true
    outputs.upToDateWhen { false }
}
tasks.withType<Test>().configureEach {
    dependsOn(buildSessionHost)
    systemProperty("java.library.path", sessionHostDir.absolutePath)
}

// The APK packages the Rust session per ABI from `composeApp/src/androidMain/jniLibs`, and until
// now nothing rebuilt those files: `build-session.sh android` was a step someone had to remember.
// A stale `.so` is invisible - the app installs, connects, and every session fix is simply absent,
// which is how a four-hour-old library was blamed on the radio. Always run it before packaging;
// cargo is incremental, so the common case is a one-second no-op.
val buildSessionAndroid = tasks.register<Exec>("buildSessionAndroid") {
    workingDir = rootProject.layout.projectDirectory.dir("native").asFile
    environment("JAVA_HOME", System.getProperty("java.home"))
    commandLine("./build-session.sh", "android")
    outputs.upToDateWhen { false }
}
tasks.matching { it.name.startsWith("merge") && it.name.contains("JniLib") }.configureEach {
    dependsOn(buildSessionAndroid)
}
