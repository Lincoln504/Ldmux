# R8 rules for the release build.
#
# R8 is on for `release` (`isMinifyEnabled` and `isShrinkResources` in
# `build.gradle.kts`). Everything below is a boundary R8 cannot see through, so each
# entry is a place the app would break in a way that only shows up in a release build:
# a JNI call to a method that was renamed, a class name that is serialised, or a
# Compose parameter reached by reflection. Nothing here is precautionary.

# ---------------------------------------------------------------------------
# JNI: the C entry points are spelled `Java_io_ldmux_mobile_<pkg>_<class>_<method>`.
# A renamed class or method makes the dynamic lookup fail at run time with
# `UnsatisfiedLinkError`, which is exactly what R8 does to a name it considers unused.
# Both JNI classes are kept whole: their method names are the C symbol names.
-keep class io.ldmux.mobile.term.NativeTerminal { *; }
-keep class io.ldmux.mobile.mesh.NativeMeshSession { *; }
# The library loader reads these names from `System.loadLibrary`, which R8 cannot see.
-keepclasseswithmembernames,includedescriptorclasses class * {
    native <methods>;
}

# ---------------------------------------------------------------------------
# No `kotlinx.serialization` rules, and that is a decision rather than an omission:
# the app parses JSON with the tree API (`Json.parseToJsonElement`), so no
# `@Serializable` class and no generated `$$serializer` exists to be renamed. If one
# is ever added, the rule it needs has to be added here in the same commit, because
# the failure is a `SerializationException` at the first message and not a build error.
# Annotations are kept anyway: `@JvmStatic`-style metadata is read by the JNI side's
# `GetMethodID` fallback paths.
-keepattributes *Annotation*, InnerClasses

# ---------------------------------------------------------------------------
# Compose. The runtime instantiates composables and reflection-reads parameter names
# for `@Composable` lambdas; stripping metadata makes a release build crash with
# "Function ... could not be found" only on screens that use a defaulted parameter.
-keepclassmembers class * {
    @androidx.compose.runtime.Composable <methods>;
}
-dontwarn androidx.compose.**

# ---------------------------------------------------------------------------
# Android components R8 must not rename: the manifest refers to them by name, and
# `Class.forName` is used by the framework, not by this app.
-keep class io.ldmux.mobile.MainActivity { *; }
-keep class io.ldmux.mobile.LdmuxService { *; }
