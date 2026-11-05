import org.jetbrains.kotlin.gradle.dsl.JvmTarget
import java.util.Properties

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

// The signing inputs, read once. `keystore.properties` is the local form; the four
// `LDMUX_KEYSTORE*` variables are the CI form. Absent is not an error for a debug build,
// and it is not an error here either: the release task is the place that must fail.
val keystoreProperties = Properties().apply {
    val file = rootProject.file("keystore.properties")
    if (file.exists()) file.inputStream().use { load(it) }
}

fun signingValue(key: String, env: String): String? =
    keystoreProperties.getProperty(key) ?: System.getenv(env)

val releaseStorePath: String? = signingValue("storeFile", "LDMUX_KEYSTORE")
val releaseStorePassword: String? = signingValue("storePassword", "LDMUX_KEYSTORE_PASSWORD")
val releaseKeyAlias: String? = signingValue("keyAlias", "LDMUX_KEY_ALIAS")
val releaseKeyPassword: String? = signingValue("keyPassword", "LDMUX_KEY_PASSWORD")
val releaseSigningAvailable: Boolean = listOf(
    releaseStorePath, releaseStorePassword, releaseKeyAlias, releaseKeyPassword,
).all { !it.isNullOrBlank() }

/** The repository's commit count, or null when this is not a git checkout. */
fun gitCommitCount(): Int? = runCatching {
    val process = ProcessBuilder("git", "rev-list", "--count", "HEAD")
        .directory(rootProject.projectDir)
        .redirectErrorStream(true)
        .start()
    val text = process.inputStream.bufferedReader().readText().trim()
    if (process.waitFor() == 0) text.toIntOrNull() else null
}.getOrNull()

android {
    namespace = "io.ldmux.mobile"
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
        applicationId = "io.ldmux.mobile"
        minSdk = 28
        targetSdk = 36
        // Monotonic without a human remembering: the commit count only goes up. Play
        // rejects any upload whose versionCode it has already seen, so a literal 1 was
        // a release that could be made exactly once. `LDMUX_VERSION_CODE` overrides it
        // for a rebuild of an already-uploaded commit.
        versionCode = (System.getenv("LDMUX_VERSION_CODE")?.toIntOrNull())
            ?: gitCommitCount()
            ?: 1
        versionName = System.getenv("LDMUX_VERSION_NAME") ?: "0.1.0"
    }

    packaging {
        resources.excludes += "/META-INF/{AL2.0,LGPL2.1}"
    }

    // **Release signing is never in the repository.** Four values come from
    // `packages/mobile/keystore.properties` (gitignored) or from the environment, and a
    // release bundle built without them is unsigned, which Play will not accept. The
    // environment route is what CI uses, so the keystore can live in a secret rather than
    // on a workstation.
    signingConfigs {
        if (releaseSigningAvailable) {
            create("release") {
                storeFile = file(releaseStorePath!!)
                storePassword = releaseStorePassword
                keyAlias = releaseKeyAlias
                keyPassword = releaseKeyPassword
            }
        }
    }

    buildTypes {
        getByName("release") {
            isMinifyEnabled = false
            if (releaseSigningAvailable) {
                signingConfig = signingConfigs.getByName("release")
            }
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

// A release bundle with no signing config is not a release: Play rejects it, and the
// failure is silent until the upload. Fail where the mistake is made instead.
tasks.matching { it.name == "bundleRelease" || it.name == "assembleRelease" }.configureEach {
    doFirst {
        if (!releaseSigningAvailable) {
            throw GradleException(
                "release signing is not configured: put storeFile/storePassword/keyAlias/keyPassword " +
                    "in packages/mobile/keystore.properties, or set LDMUX_KEYSTORE, " +
                    "LDMUX_KEYSTORE_PASSWORD, LDMUX_KEY_ALIAS and LDMUX_KEY_PASSWORD",
            )
        }
    }
}
