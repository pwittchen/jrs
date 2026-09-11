// The Shadow plugin: `shadowJar` is `jrs package --fat`, and its `relocate`
// calls become [package.relocate], the closure's exclusions included.
plugins {
    java
    application
    id("com.gradleup.shadow") version "8.3.5"
}

version = "1.0.0"

repositories {
    mavenCentral()
}

dependencies {
    implementation("com.google.guava:guava:33.0.0-jre")
    implementation("org.slf4j:slf4j-api:2.0.13")
}

application {
    mainClass = "com.example.Main"
}

tasks.shadowJar {
    mergeServiceFiles()
    relocate("com.google.common", "com.example.shaded.guava")
    relocate("org.slf4j", "com.example.shaded.slf4j") {
        exclude("org.slf4j.impl.**")
        exclude("org.slf4j.Marker")
    }
}
