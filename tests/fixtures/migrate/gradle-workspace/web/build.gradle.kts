plugins {
    application
    id("com.github.ben-manes.versions") version "0.52.0"
}

application {
    mainClass.set("com.example.shop.web.Main")
}

dependencies {
    implementation(project(":core"))
}
