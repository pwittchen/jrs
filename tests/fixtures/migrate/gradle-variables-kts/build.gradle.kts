// gradle-variables in the Kotlin DSL: `val`, `extra` and `by project` where
// the Groovy build has `ext` and `def`, and named arguments for map notation.
buildscript {
    val springBootVersion by extra("3.5.4")
    repositories {
        mavenCentral()
    }
    dependencies {
        classpath("org.springframework.boot:spring-boot-gradle-plugin:$springBootVersion")
    }
}

plugins {
    java
    application
}

apply(plugin = "org.springframework.boot")
apply(plugin = "io.spring.dependency-management")

group = "com.example"
version = "1.4.0"

java {
    toolchain {
        languageVersion = JavaLanguageVersion.of(21)
    }
}

val guavaVersion = "33.4.8-jre"
val slf4jVersion: String = "2.0.17"
extra["springCloudVersion"] = "2025.0.0"
extra["nettyVersion"] = "4.2.4.Final"
val lombokVersion = "1.18.38"
val junitVersion = "5.13.4"
val junitBomVersion = junitVersion
val testcontainersVersion = "1.21.3"
val jacksonVersion: String by project

// Neither of these is a literal set once.
val buildNumber = System.getenv("BUILD_NUMBER") ?: "dev"
var h2Version = "2.3.232"
if (hasProperty("legacyH2")) {
    h2Version = "2.2.224"
}

repositories {
    mavenCentral()
}

dependencyManagement {
    imports {
        mavenBom("org.springframework.cloud:spring-cloud-dependencies:${property("springCloudVersion")}")
    }
}

dependencies {
    implementation(platform("org.testcontainers:testcontainers-bom:$testcontainersVersion"))
    implementation(enforcedPlatform("org.junit:junit-bom:${junitBomVersion}"))

    implementation("org.springframework.boot:spring-boot-starter-web")
    implementation("com.google.guava:guava:$guavaVersion")
    implementation("org.slf4j:slf4j-api:${slf4jVersion}")
    implementation("io.netty:netty-codec-http:${rootProject.extra["nettyVersion"]}")
    runtimeOnly("io.netty:netty-transport-native-epoll:${project.extra["nettyVersion"]}:linux-x86_64")
    implementation("com.fasterxml.jackson.core:jackson-databind:$jacksonVersion")
    compileOnly(group = "org.projectlombok", name = "lombok", version = lombokVersion)
    annotationProcessor(group = "org.projectlombok", name = "lombok", version = "$lombokVersion")

    constraints {
        implementation("org.yaml:snakeyaml:${project.property("snakeyamlVersion")}")
    }

    // Left out, never written without a version.
    implementation(group = "com.example", name = "build-info", version = buildNumber)
    testRuntimeOnly("com.h2database:h2:$h2Version")

    testImplementation("org.junit.jupiter:junit-jupiter")
    testImplementation("org.testcontainers:postgresql")
}

application {
    mainClass = "com.example.VariablesApp"
}
