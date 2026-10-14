plugins {
    application
    kotlin("jvm") version "2.2.0"
    id("io.ktor.plugin") version "3.2.3"
}

group = "io.ktor.samples"
version = "0.0.1"

application {
    mainClass.set("io.ktor.samples.ApplicationKt")
}

repositories {
    mavenCentral()
}

dependencies {
    implementation("io.ktor:ktor-server-core")
    implementation("io.ktor:ktor-server-netty")
    implementation("io.ktor:ktor-serialization-kotlinx-json:3.2.0")
    implementation("ch.qos.logback:logback-classic:1.5.18")
    testImplementation("io.ktor:ktor-server-test-host")
    testImplementation(kotlin("test"))
}
