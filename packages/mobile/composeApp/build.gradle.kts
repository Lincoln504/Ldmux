import org.jetbrains.kotlin.gradle.dsl.JvmTarget
import java.util.Properties
import java.util.zip.ZipFile

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

    // **The licence texts ship inside the artifact.** MIT, BSD-3-Clause and Apache-2.0 all require
    // the notice to travel with a distributed binary; a Markdown file in a repository the user
    // never opens does not discharge that. `scripts/collect-licenses.sh` writes the tree, and the
    // release tasks run it first (see `scripts/android.sh`), so a bundle cannot be produced without
    // the notices it owes.
    sourceSets["main"].assets.srcDir(rootDir.resolve("../..").resolve("dist"))
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
        // **The ABIs this app ships, declared.** `libldmux_session.so` and
        // `libldmux_terminal.so` are built for 64-bit only. Without this, a dependency that ships
        // 32-bit code (`androidx.graphics.path` does) puts `armeabi-v7a` and `x86` in the bundle,
        // Play offers that split to a 32-bit device, and the app installs, starts, and dies with
        // `UnsatisfiedLinkError` on the first session. Measured in the 0.1.0 bundle: four ABI
        // directories, our two libraries in two of them.
        ndk {
            abiFilters += listOf("arm64-v8a", "x86_64")
        }
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

// **Both native libraries, or no release.** `jniLibs/` is gitignored, and the Gradle tasks above
// build only the Rust session; `libldmux_terminal.so` comes from `scripts/build-libghostty-vt.sh`,
// which `scripts/android.sh` runs but a bare `./gradlew bundleRelease` does not. The result installs,
// starts, and dies with `UnsatisfiedLinkError` on the first session - a release artifact that cannot
// open a shell. Checked here, where the mistake is made.
val requiredNativeLibs = listOf("libldmux_session.so", "libldmux_terminal.so")
val abiDirs = listOf("arm64-v8a", "x86_64")
tasks.matching { it.name == "bundleRelease" || it.name == "assembleRelease" }.configureEach {
    doFirst {
        val root = file("src/androidMain/jniLibs")
        val missing = abiDirs.flatMap { abi -> requiredNativeLibs.map { "$abi/$it" } }
            .filter { !root.resolve(it).isFile }
        if (missing.isNotEmpty()) {
            throw GradleException(
                "native libraries missing from src/androidMain/jniLibs: ${missing.joinToString(", ")}. " +
                    "Run scripts/android.sh release or bundle, which builds both; " +
                    "scripts/build-libghostty-vt.sh produces the terminal engine.",
            )
        }
    }
}

// **The bundle that was produced, inspected.** The check above reads `jniLibs`, which cannot know
// what Gradle merged from dependencies; this one opens the artifact and refuses any ABI outside the
// declared list, and any listed ABI without both libraries. A packaging rule that is only asserted
// against its own input is an assumption about the output.
tasks.matching { it.name == "bundleRelease" || it.name == "assembleRelease" }.configureEach {
    doFirst {
        // `scripts/collect-licenses.sh` writes this tree and `android.sh` runs it first. A
        // missing tree means the artifact would ship without the notices MIT, BSD-3-Clause and
        // Apache-2.0 require, so refuse here rather than trusting the caller.
        val licenses = rootDir.resolve("../..").resolve("dist/licenses")
        val notices = licenses.resolve("THIRD-PARTY-NOTICES.md")
        if (!notices.isFile) {
            throw GradleException(
                "licence texts missing from ${licenses.path}: run scripts/collect-licenses.sh " +
                    "before packaging, or use scripts/android.sh release/bundle which does it.",
            )
        }
    }
}

tasks.matching { it.name == "bundleRelease" }.configureEach {
    doLast {
        val aab = file("build/outputs/bundle/release/composeApp-release.aab")
        if (!aab.isFile) return@doLast
        ZipFile(aab).use { zip ->
            val shipped = zip.entries().asSequence()
                .map { it.name }
                .filter { it.startsWith("base/lib/") && it.endsWith(".so") }
                .map { it.removePrefix("base/lib/").substringBefore('/') }
                .distinct()
                .toList()
            val extra = shipped.filter { it !in abiDirs }
            if (extra.isNotEmpty()) {
                throw GradleException(
                    "the bundle ships ABIs that the app has no libraries for: ${extra.joinToString(", ")}. " +
                        "Declare the shipped ABIs in `ndk { abiFilters }`.",
                )
            }
            val missing = abiDirs.flatMap { abi -> requiredNativeLibs.map { "$abi/$it" } }
                .filter { path -> zip.getEntry("base/lib/$path") == null }
            if (missing.isNotEmpty()) {
                throw GradleException("the bundle is missing: ${missing.joinToString(", ")}")
            }
        }
    }
}

// **A release cannot be built with the version-code fallback.** `versionCode` comes from
// `git rev-list --count HEAD`, so a build from a tarball (no `.git`) would silently produce 1 —
// the number that has already been uploaded, rejected by Play, and identical for every such
// build. The fallback stays for debug builds, where nothing reads it.
val versionCodeKnown: Boolean = (System.getenv("LDMUX_VERSION_CODE")?.toIntOrNull()) != null ||
    gitCommitCount() != null
tasks.matching { it.name == "bundleRelease" || it.name == "assembleRelease" }.configureEach {
    doFirst {
        if (!versionCodeKnown) {
            throw GradleException(
                "release versionCode is unknown: this is not a git checkout, so the commit count " +
                    "cannot be read. Set LDMUX_VERSION_CODE (for example the number the last upload " +
                    "used, plus one), or build from the repository.",
            )
        }
    }
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
