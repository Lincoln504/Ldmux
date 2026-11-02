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
include(":composeApp", ":libmeshcore", ":libmeshcore-android")

// Vendored from meshmore-sns at 7386999; see vendor/README.md.
project(":libmeshcore").projectDir = file("vendor/libmeshcore")
project(":libmeshcore-android").projectDir = file("vendor/libmeshcore-android")
