// Kotlin on the JVM as the Gradle Kotlin plugin sets it up: the plugin's
// version is the compiler's, `kotlin("...")` names its modules, and the
// toolchain goes through the `kotlin { }` extension.
plugins {
    kotlin("jvm") version "2.2.0"
    kotlin("plugin.spring") version "2.2.0"
    application
}

group = "com.example"
version = "0.5.0"

repositories {
    mavenCentral()
}

dependencies {
    implementation(kotlin("stdlib"))
    implementation(kotlin("reflect"))
    implementation("com.fasterxml.jackson.module:jackson-module-kotlin:2.19.1")
    testImplementation(kotlin("test"))
}

kotlin {
    jvmToolchain(21)
}

application {
    mainClass = "com.example.MainKt"
}

tasks.test {
    useJUnitPlatform()
}
