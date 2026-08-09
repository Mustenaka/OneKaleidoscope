import java.util.zip.ZipFile
import org.jetbrains.kotlin.gradle.dsl.JvmTarget

plugins {
    alias(libs.plugins.android.library)
    alias(libs.plugins.kotlin.android)
}

val workspaceRoot = rootDir.resolve("../..").canonicalFile
val generatedRoot = layout.buildDirectory.dir("generated")
val generatedKotlin = generatedRoot.map { it.dir("uniffi/kotlin") }
val generatedJniLibs = generatedRoot.map { it.dir("jniLibs") }
val cargoWorkspaceManifests = fileTree(workspaceRoot) {
    include("Cargo.toml")
    include("crates/*/Cargo.toml")
    include("spikes/*/Cargo.toml")
    include("xtask/Cargo.toml")
}
val rustBuildInputs = files(
    workspaceRoot.resolve("Cargo.lock"),
    workspaceRoot.resolve("rust-toolchain.toml"),
    cargoWorkspaceManifests,
    fileTree(workspaceRoot.resolve("crates/kaleido-core")) {
        include("src/**")
        include("build.rs")
    },
    fileTree(workspaceRoot.resolve("crates/kaleido-proto")) {
        include("src/**")
        include("build.rs")
    },
    fileTree(workspaceRoot.resolve("crates/kaleido-transport")) {
        include("src/**")
        include("build.rs")
    },
)

val hostLibraryName = when {
    System.getProperty("os.name").startsWith("Windows", ignoreCase = true) -> "kaleido_core.dll"
    System.getProperty("os.name").startsWith("Mac", ignoreCase = true) -> "libkaleido_core.dylib"
    else -> "libkaleido_core.so"
}
val hostLibrary = workspaceRoot.resolve("target/release/$hostLibraryName")

android {
    namespace = "com.onekaleidoscope.core"
    compileSdk = 35

    defaultConfig {
        minSdk = 26
        consumerProguardFiles("consumer-rules.pro")
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    sourceSets.named("main") {
        java.srcDir(generatedKotlin)
        jniLibs.srcDir(generatedJniLibs)
    }

    buildFeatures {
        buildConfig = false
    }
}

kotlin {
    compilerOptions {
        jvmTarget.set(JvmTarget.JVM_17)
    }
}

dependencies {
    implementation(libs.kotlinx.coroutines.android)
    implementation(libs.jna.android) {
        artifact {
            type = "aar"
        }
    }
    testImplementation(libs.junit4)
}

val buildHostKaleidoCore by tasks.registering(Exec::class) {
    group = "rust"
    description = "Builds the host kaleido-core cdylib used as UniFFI metadata input."
    workingDir(workspaceRoot)
    commandLine("cargo", "build", "--locked", "--release", "-p", "kaleido-core", "--lib")
    inputs.files(rustBuildInputs)
    outputs.file(hostLibrary)
}

val generateUniFfiKotlin by tasks.registering(Exec::class) {
    group = "rust"
    description = "Generates Kotlin bindings from kaleido-core's embedded UniFFI metadata."
    dependsOn(buildHostKaleidoCore)
    workingDir(workspaceRoot)
    val outputDirectory = generatedKotlin.get().asFile
    commandLine(
        "cargo",
        "run",
        "--locked",
        "--release",
        "-p",
        "kaleido-core",
        "--bin",
        "uniffi-bindgen",
        "--",
        "generate",
        "--language",
        "kotlin",
        "--out-dir",
        outputDirectory,
        hostLibrary,
    )
    inputs.files(rustBuildInputs)
    inputs.file(hostLibrary)
    outputs.dir(outputDirectory)
    doFirst {
        delete(outputDirectory)
    }
}

val buildRustAndroid by tasks.registering(Exec::class) {
    group = "rust"
    description = "Builds kaleido-core for the two Android ABIs supported by R3."
    workingDir(workspaceRoot)
    val outputDirectory = generatedJniLibs.get().asFile
    commandLine(
        "cargo",
        "ndk",
        "-P",
        "26",
        "-t",
        "arm64-v8a",
        "-t",
        "x86_64",
        "-o",
        outputDirectory,
        "build",
        "--locked",
        "--release",
        "-p",
        "kaleido-core",
        "--lib",
    )
    inputs.files(rustBuildInputs)
    outputs.dir(outputDirectory)
    doFirst {
        delete(outputDirectory)
    }
}

val verifyCoreAndroidInputs by tasks.registering {
    group = "verification"
    description = "Rejects missing bindings or either required Android native library."
    dependsOn(generateUniFfiKotlin, buildRustAndroid)
    doLast {
        val required = listOf(
            generatedKotlin.get().file("uniffi/kaleido_core/kaleido_core.kt").asFile,
            generatedKotlin.get().file("uniffi/kaleido_proto/kaleido_proto.kt").asFile,
            generatedJniLibs.get().file("arm64-v8a/libkaleido_core.so").asFile,
            generatedJniLibs.get().file("x86_64/libkaleido_core.so").asFile,
        )
        required.forEach { artifact ->
            check(artifact.isFile && artifact.length() > 0L) {
                "required generated Android input is missing or empty: ${artifact.name}"
            }
        }
    }
}

val verifyCoreAndroidAar by tasks.registering {
    group = "verification"
    description = "Checks that the release AAR packages both Rust shared libraries."
    dependsOn("assembleRelease")
    doLast {
        val aar = layout.buildDirectory.file("outputs/aar/core-android-release.aar").get().asFile
        check(aar.isFile && aar.length() > 0L) { "release AAR was not produced" }
        ZipFile(aar).use { archive ->
            val expectedAbis = setOf("arm64-v8a", "x86_64")
            val packagedAbis = archive.entries().asSequence()
                .map { it.name }
                .filter { it.startsWith("jni/") }
                .map { it.substringAfter("jni/").substringBefore('/') }
                .filter { it.isNotEmpty() }
                .toSet()
            check(packagedAbis == expectedAbis) {
                "release AAR must contain exactly $expectedAbis, found $packagedAbis"
            }
            listOf(
                "jni/arm64-v8a/libkaleido_core.so",
                "jni/x86_64/libkaleido_core.so",
                "classes.jar",
            ).forEach { entry ->
                val artifact = archive.getEntry(entry)
                check(artifact != null && artifact.size > 0L) {
                    "release AAR is missing or contains an empty $entry"
                }
            }
        }
    }
}

tasks.named("preBuild").configure {
    dependsOn(verifyCoreAndroidInputs)
}

tasks.named("check").configure {
    dependsOn(verifyCoreAndroidInputs)
}
