import org.jetbrains.kotlin.gradle.dsl.JvmTarget

plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.android)
    alias(libs.plugins.kotlin.compose)
    alias(libs.plugins.kotlin.serialization)
}

// ── Rust cross-build wiring ────────────────────────────────────────────────
//
// The client core is a Rust cdylib. Gradle does not know how to build it, so
// these tasks shell out to cargo-ndk and drop the resulting .so straight into
// jniLibs, where AGP packages it like any other native library.

/// ABIs to build, from gradle.properties. Each one is a full Rust build of the
/// core including libopus and libvpx, so the default is arm64-v8a alone.
val conquerdAbis: List<String> =
    (providers.gradleProperty("conquerd.abis").orNull ?: "arm64-v8a")
        .split(",")
        .map { it.trim() }
        .filter { it.isNotEmpty() }

/// Android API level the Rust side compiles against. Must match `minSdk`.
val conquerdNdkApi: String = providers.gradleProperty("conquerd.ndkApi").orNull ?: "26"

/// Overridable so a machine with a non-PATH toolchain can point at its own.
val cargoExecutable: String = providers.gradleProperty("conquerd.cargo").orNull ?: "cargo"

/// Keystore that signs the APK, when CI supplies one.
///
/// Android replaces an installed app only if the replacement carries the same
/// signature, so a build meant to land on a device already running ConquerD
/// has to be signed with the key that signed what is there now. An APK signed
/// with a CI runner's own throwaway debug keystore cannot be installed over it
/// at all - only after an uninstall, and an uninstall takes app-private
/// storage with it: the identity key and the whole message store, with
/// `allowBackup` off and no copy anywhere else.
///
/// Unset on a developer machine, where the build falls through to the debug
/// keystore Gradle manages itself. That is what has always signed local
/// builds, so it is also what the CI secret should hold.
val signingKeystore: String? = providers.environmentVariable("CONQUERD_KEYSTORE").orNull

/// Passwords for [signingKeystore]. The defaults are the published
/// debug-keystore credentials, so reusing a debug keystore needs no secret
/// beyond the file itself; a real release keystore overrides all three.
val signingStorePassword: String =
    providers.environmentVariable("CONQUERD_KEYSTORE_PASSWORD").orNull ?: "android"
val signingKeyAlias: String =
    providers.environmentVariable("CONQUERD_KEY_ALIAS").orNull ?: "androiddebugkey"
val signingKeyPassword: String =
    providers.environmentVariable("CONQUERD_KEY_PASSWORD").orNull ?: "android"

/// Build stamp appended to the version name, so a side-loaded APK can be
/// identified from the device Settings screen.
///
/// Deliberately not `versionCode`: Android refuses to install an APK whose
/// versionCode is below the installed one, so bumping it per CI run would mean
/// a later local build could only be installed by uninstalling first - which
/// costs the identity key and the message store. versionName carries no such
/// rule, so it is the safe place to put a build number.
val conquerdBuildStamp: String =
    providers.environmentVariable("CONQUERD_BUILD_ID").orNull?.let { "-$it" } ?: ""

val rustCrateDir = rootProject.layout.projectDirectory.dir("../rust/conquerd-android")
val jniLibsDir = layout.projectDirectory.dir("src/main/jniLibs")

/// Register one cargo-ndk invocation.
///
/// Android's debug build maps to cargo's dev profile and release to release:
/// a release APK carrying a debug-profile core would be unusably slow through
/// the Opus and VP8 paths, which are pure C compiled without SIMD.
fun registerCargoBuild(taskName: String, releaseProfile: Boolean) =
    tasks.register<Exec>(taskName) {
        group = "build"
        description = "Cross-compile the ConquerD client core for Android (" +
            (if (releaseProfile) "release" else "debug") + ")."

        workingDir = rustCrateDir.asFile

        val arguments = mutableListOf("ndk")
        conquerdAbis.forEach { abi -> arguments += listOf("-t", abi) }
        arguments += listOf(
            "--platform", conquerdNdkApi,
            // cargo-ndk writes <dir>/<abi>/lib<name>.so itself, which is
            // exactly the layout AGP expects from a jniLibs source dir.
            "-o", jniLibsDir.asFile.absolutePath,
            "build", "--lib",
        )
        if (releaseProfile) arguments += "--release"

        commandLine(listOf(cargoExecutable) + arguments)

        // cargo-ndk locates the NDK through these; AGP already resolved the
        // SDK path, so don't make the developer set them a second time.
        environment("ANDROID_HOME", android.sdkDirectory.absolutePath)
        environment("ANDROID_NDK_HOME", android.ndkDirectory.absolutePath)

        // Keep Android object files out of the desktop target/ tree, so a
        // native `cargo build` afterwards doesn't rebuild the world.
        environment(
            "CARGO_TARGET_DIR",
            rootProject.layout.projectDirectory.dir("../rust/target-android").asFile.absolutePath,
        )

        inputs.dir(rustCrateDir)
        // The core itself, not just the JNI shim — a change in either has to
        // rebuild the .so.
        inputs.dir(rootProject.layout.projectDirectory.dir("../rust/conquerd-client/src"))
        outputs.dir(jniLibsDir)
    }

val cargoBuildDebug = registerCargoBuild("cargoBuildDebug", releaseProfile = false)
val cargoBuildRelease = registerCargoBuild("cargoBuildRelease", releaseProfile = true)

android {
    namespace = "com.conquerd.client"
    compileSdk = 36

    // Pinned rather than "whatever is installed": the NDK version decides the
    // libc symbols the core links against, so a silent bump is a silent change
    // to which devices the APK runs on.
    ndkVersion = "28.2.13676358"

    defaultConfig {
        applicationId = "com.conquerd.client"
        minSdk = 26
        targetSdk = 36
        versionCode = 1
        versionName = "1.0.0$conquerdBuildStamp"

        ndk {
            abiFilters += conquerdAbis
        }
    }

    // Left alone on a developer machine, where AGP's own debug keystore signs
    // the build exactly as it always has. CI replaces it so the APK it
    // produces is installable over what is already on a test device. The same
    // env vars sign `bundleRelease` when set, which is what Play Console
    // accepts; an unset machine still produces an unsigned release bundle.
    if (signingKeystore != null) {
        signingConfigs {
            getByName("debug") {
                storeFile = file(signingKeystore)
                storePassword = signingStorePassword
                keyAlias = signingKeyAlias
                keyPassword = signingKeyPassword
            }
            create("release") {
                storeFile = file(signingKeystore)
                storePassword = signingStorePassword
                keyAlias = signingKeyAlias
                keyPassword = signingKeyPassword
            }
        }
    }

    buildTypes {
        debug {
            isMinifyEnabled = false
        }
        release {
            isMinifyEnabled = true
            isShrinkResources = true
            if (signingKeystore != null) {
                signingConfig = signingConfigs.getByName("release")
            }
            proguardFiles(
                getDefaultProguardFile("proguard-android-optimize.txt"),
                "proguard-rules.pro",
            )
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    buildFeatures {
        compose = true
    }

    packaging {
        resources {
            excludes += "/META-INF/{AL2.0,LGPL2.1}"
        }
        jniLibs {
            // Uncompressed and page-aligned, so the loader maps the core
            // straight from the APK instead of extracting it on first launch.
            useLegacyPackaging = false
        }
    }
}

kotlin {
    compilerOptions {
        jvmTarget.set(JvmTarget.JVM_17)
    }
}

// Build the core before anything tries to package it.
androidComponents {
    onVariants { variant ->
        val cargoTask = if (variant.buildType == "release") cargoBuildRelease else cargoBuildDebug
        project.tasks.matching { it.name == "merge${variant.name.replaceFirstChar(Char::uppercase)}JniLibFolders" }
            .configureEach { dependsOn(cargoTask) }
    }
}

dependencies {
    implementation(libs.androidx.core.ktx)
    implementation(libs.androidx.lifecycle.runtime.ktx)
    implementation(libs.androidx.lifecycle.process)
    implementation(libs.androidx.lifecycle.viewmodel.compose)
    implementation(libs.androidx.activity.compose)
    implementation(libs.kotlinx.coroutines.android)
    implementation(libs.kotlinx.serialization.json)

    // CameraX rather than raw Camera2: it handles the device-specific mess
    // (orientation, aspect ratios, buffer formats) that a P2P client has no
    // business re-solving per handset.
    implementation(libs.androidx.camera.core)
    implementation(libs.androidx.camera.camera2)
    implementation(libs.androidx.camera.lifecycle)

    implementation(platform(libs.androidx.compose.bom))
    implementation(libs.androidx.compose.ui)
    implementation(libs.androidx.compose.ui.graphics)
    implementation(libs.androidx.compose.ui.tooling.preview)
    implementation(libs.androidx.compose.material3)
    debugImplementation(libs.androidx.compose.ui.tooling)

    testImplementation(libs.junit)
}
