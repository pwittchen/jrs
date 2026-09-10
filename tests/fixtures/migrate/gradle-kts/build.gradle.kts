// The Kotlin DSL, with what jrs.toml can express beyond `group:artifact =
// version`: exclusions, classifiers, compile-only and JVM arguments.
plugins {
    java
    application
}

group = "com.example"
version = "2.1.0"

java {
    toolchain {
        languageVersion = JavaLanguageVersion.of(17)
    }
}

repositories {
    mavenCentral()
}

dependencies {
    implementation("com.google.guava:guava:33.0.0-jre") {
        exclude(group = "com.google.code.findbugs", module = "jsr305")
    }
    implementation("org.lwjgl:lwjgl:3.3.3:natives-linux")
    implementation("com.fasterxml.jackson.core:jackson-databind:2.17.0") { isTransitive = false }
    compileOnly("org.projectlombok:lombok:1.18.34")
    annotationProcessor("org.projectlombok:lombok:1.18.34")
    runtimeOnly("org.postgresql:postgresql:42.7.3")
    testImplementation("junit:junit:4.13.2")
}

application {
    mainClass.set("com.example.App")
    applicationDefaultJvmArgs = listOf("-Xmx1g", "-Dapp.mode=prod")
}

tasks.test {
    useJUnit()
    jvmArgs("-Xmx256m")
    systemProperty("env", "test")
}
