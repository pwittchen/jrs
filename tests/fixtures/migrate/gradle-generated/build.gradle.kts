import kotlin.random.Random.Default.nextInt

plugins {
  alias(libs.plugins.kotlin.jvm)
  alias(libs.plugins.kotlin.plugin.serialization)
  alias(libs.plugins.openapi)
}

version = "1.2.0"

val signKey = "0123abcd"
val jsToml = rootProject.file("gradle/libs.versions.js.toml")
val openApiOutputDir = layout.buildDirectory.dir("generated/sources/openapi")

application {
  mainClass = "com.example.AppKt"
}

configurations.all {
  resolutionStrategy {
    force(libs.kotlin.logging)
  }
}

dependencies {
  implementation(libs.bundles.main)
  testImplementation(libs.bundles.test)
}

tasks.test {
  useJUnitPlatform()
  val testPort = nextInt(8100, 8999)
  environment("TEST_PORT", testPort)
  environment("API_URL", "http://localhost:$testPort/api")
  environment("PAGE_SIZE", 13)
  environment("LIMIT", 10_000)
  environment("SIGN_KEY", signKey)
}

kotlin {
  jvmToolchain(21)
  compilerOptions {
    javaParameters = true
  }
}

val generateUrls =
  tasks.register("generateUrls") {
    val outputDir = layout.buildDirectory.dir("generated/sources/urls/kotlin/com/example")
    inputs.file(jsToml)
    outputs.dir(outputDir)
    doLast {
      outputDir.get().asFile.mkdirs()
    }
  }

val styles =
  tasks.register("styles") {
    val outputFile = layout.buildDirectory.file("generated/resources/styles/app.css")
    inputs.dir(layout.projectDirectory.dir("src"))
    outputs.file(outputFile)
    doLast { println("styles") }
  }

sourceSets {
  main {
    kotlin {
      srcDir(layout.buildDirectory.dir("generated/sources/urls/kotlin"))
      srcDir(openApiOutputDir.map { it.dir("src/main/kotlin") })
    }
    resources {
      srcDir(layout.buildDirectory.dir("generated/resources/styles"))
    }
  }
}

tasks.named("processResources") {
  dependsOn(styles)
}

tasks.named("compileKotlin") {
  dependsOn(generateUrls, tasks.openApiGenerate)
}

openApiGenerate {
  generatorName.set("kotlin")
  inputSpec.set("$rootDir/api/spec.yml")
  outputDir.set(openApiOutputDir.map { it.asFile.absolutePath })
  modelPackage.set("com.example.api")
  globalProperties.set(
    mapOf(
      "models" to "", // every model
      "apis" to "false",
    ),
  )
  configOptions.set(mapOf("serializationLibrary" to "kotlinx_serialization"))
}
