pluginManagement {
    repositories {
        google {
            content {
                includeGroupByRegex("com\\.android.*")
                includeGroupByRegex("com\\.google.*")
                includeGroupByRegex("androidx.*")
            }
        }
        mavenCentral()
        gradlePluginPortal()
    }
}

dependencyResolutionManagement {
    repositories {
        google()
        mavenCentral()
    }
}

rootProject.name = "ldmux-mobile"
include(":composeApp", ":ble")

// The BLE transport: GATT connection, MTU, serialized writes and the scanner's UUIDs. **No
// protocol** — the companion commands, frames and session are Rust behind `ldmux_client_*`
// (`docs/RADIO-PLAN.md` §8.1, `A2`). Originally vendored from meshmore-sns at 7386999; what is
// left is the part Android owns.
project(":ble").projectDir = file("ble")
