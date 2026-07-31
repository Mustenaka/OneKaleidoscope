plugins {
    kotlin("jvm") version "2.2.20"
}

dependencies {
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-core:1.11.0")
    implementation("net.java.dev.jna:jna:5.19.1")
}

kotlin {
    jvmToolchain(22)
    sourceSets {
        main {
            kotlin.srcDir("../../../../target/uniffi/kotlin")
        }
    }
}
