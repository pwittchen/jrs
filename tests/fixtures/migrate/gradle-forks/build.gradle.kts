// The test task spread over several JVMs, as Gradle's performance guide
// suggests: `maxParallelForks` becomes `test.forks`. `forkEvery`, which
// restarts a JVM after so many test classes, has no counterpart.
plugins {
    java
}

group = "com.example"
version = "3.0.0"

java {
    toolchain {
        languageVersion = JavaLanguageVersion.of(21)
    }
}

repositories {
    mavenCentral()
}

dependencies {
    implementation("com.fasterxml.jackson.core:jackson-databind:2.19.1")
    testImplementation(platform("org.junit:junit-bom:5.13.4"))
    testImplementation("org.junit.jupiter:junit-jupiter")
    testRuntimeOnly("org.junit.platform:junit-platform-launcher")
}

tasks.test {
    useJUnitPlatform()
    maxParallelForks = 4
    forkEvery = 100
    jvmArgs("-Xmx512m")
}
