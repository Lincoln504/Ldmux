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
include(":composeApp", ":libmeshcore-android")

// Vendored from meshmore-sns at 7386999; see vendor/README.md. Only the BLE transport is left:
// the companion protocol it used to carry is in Rust now (`docs/RADIO-PLAN.md` §8.1).
project(":libmeshcore-android").projectDir = file("vendor/libmeshcore-android")
