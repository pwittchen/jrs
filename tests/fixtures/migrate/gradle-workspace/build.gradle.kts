subprojects {
    apply(plugin = "java")

    group = "com.example.shop"
    version = "2.3.0"

    repositories {
        mavenCentral()
        maven { url = uri("https://maven.example.com/releases") }
    }

    java {
        toolchain {
            languageVersion.set(JavaLanguageVersion.of(21))
        }
    }

    dependencies {
        testImplementation(libs.junit.jupiter)
    }
}
