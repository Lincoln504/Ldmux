plugins {
    `java-library`
}

group = "io.iotone.meshcore"
version = "0.1.0"

java {
    // The library is Java 17 (records/sealed types). Android consumes it through AGP
    // desugaring; the JVM tests run on the toolchain below.
    toolchain {
        languageVersion = JavaLanguageVersion.of(17)
    }
    withSourcesJar()
}

repositories {
    mavenCentral()
}

dependencies {
    // Ed25519 / X25519 via the BouncyCastle lightweight math API. The JCE provider is
    // deliberately not registered: Android ships a stripped BouncyCastle.
    api("org.bouncycastle:bcprov-jdk18on:1.81")

    testImplementation(platform("org.junit:junit-bom:5.11.4"))
    testImplementation("org.junit.jupiter:junit-jupiter")
    testRuntimeOnly("org.junit.platform:junit-platform-launcher")
    testImplementation("org.json:json:20240303")
}

tasks.test {
    useJUnitPlatform()
}
