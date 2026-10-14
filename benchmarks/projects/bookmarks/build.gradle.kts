// examples/bookmarks's jrs.toml, as Gradle: benches/compare.rs builds it
// beside the example's own sources (specs/BENCHMARKS.md §2). Boot's
// plugin compiles with -parameters, as the jrs.toml asks for, and its
// dependency management takes the BOM's versions as Maven does.
plugins {
    java
    id("org.springframework.boot") version "4.1.1"
    id("io.spring.dependency-management") version "1.1.7"
}

group = "com.example"
version = "1.0.0"

repositories {
    mavenCentral()
}

dependencies {
    implementation("org.springframework.boot:spring-boot-starter-webmvc")
    testImplementation("org.springframework.boot:spring-boot-starter-webmvc-test")
    testRuntimeOnly("org.junit.platform:junit-platform-launcher")
}

tasks.withType<JavaCompile>().configureEach {
    options.release = 17
}

tasks.test {
    useJUnitPlatform()
}
