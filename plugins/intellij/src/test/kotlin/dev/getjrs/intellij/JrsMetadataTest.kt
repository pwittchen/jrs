package dev.getjrs.intellij

import com.intellij.openapi.roots.DependencyScope
import dev.getjrs.intellij.model.JrsMetadata
import dev.getjrs.intellij.sync.JrsModelApplier
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Test

class JrsMetadataTest {
    private val json = """
        {
          "version": 1,
          "jrs": "0.9.0",
          "project": {
            "name": "app", "version": "1.0.0", "main-class": "com.example.Main", "jrs-version": null,
            "root": "/work/app", "manifest": "/work/app/jrs.toml", "lockfile": "/work/app/jrs.lock"
          },
          "jdk": { "home": "/jdk", "version": 21, "javac": "/jdk/bin/javac", "java": "/jdk/bin/java" },
          "java": { "release": 21, "target": null, "encoding": "UTF-8", "javac-args": ["-Xlint"] },
          "languages": [
            { "name": "java", "version": null, "compiler": null },
            { "name": "kotlin", "version": "2.4.20", "compiler": "org.jetbrains.kotlin:kotlin-compiler-embeddable:2.4.20" }
          ],
          "main": {
            "sources": [ { "language": "java", "dir": "/work/app/src/main/java" } ],
            "generated-sources": ["/work/app/target/generated/sources"],
            "resources": ["/work/app/src/main/resources"],
            "generated-resources": [],
            "output": "/work/app/target/classes"
          },
          "test": {
            "sources": [ { "language": "java", "dir": "/work/app/src/test/java" } ],
            "generated-sources": [], "resources": [], "generated-resources": [],
            "output": "/work/app/target/test-classes"
          },
          "target-dir": "/work/app/target",
          "jar": "/work/app/target/app-1.0.0.jar",
          "classpaths": {
            "compile": [
              { "coordinate": "g:both:1", "jar": "/c/both.jar", "sources": "/c/both-sources.jar" },
              { "coordinate": "g:provided:1", "jar": "/c/provided.jar", "sources": null }
            ],
            "runtime": [
              { "coordinate": "g:both:1", "jar": "/c/both.jar", "sources": "/c/both-sources.jar" },
              { "coordinate": "g:runtime:1", "jar": "/c/runtime.jar", "sources": null }
            ],
            "test": [
              { "coordinate": "g:both:1", "jar": "/c/both.jar", "sources": null },
              { "coordinate": "g:provided:1", "jar": "/c/provided.jar", "sources": null },
              { "coordinate": "g:runtime:1", "jar": "/c/runtime.jar", "sources": null },
              { "coordinate": "org.junit:junit-jupiter:5", "jar": "/c/junit.jar", "sources": null },
              { "coordinate": null, "jar": "/work/app/libs/local.jar", "sources": null }
            ]
          },
          "tasks": [ { "name": "gen", "description": "Generate", "hooks": ["pre-compile"] } ],
          "a-key-from-a-newer-jrs": true
        }
    """.trimIndent()

    @Test
    fun `parses every section`() {
        val m = JrsMetadata.parse(json)
        assertEquals("app", m.project.name)
        assertEquals("com.example.Main", m.project.mainClass)
        assertEquals(21, m.jdk?.version)
        assertEquals(21, m.java.release)
        assertEquals(listOf("java", "kotlin"), m.languages.map { it.name })
        assertEquals("/work/app/src/main/java", m.main.sources.single().dir)
        assertEquals(listOf("/work/app/target/generated/sources"), m.main.generatedSources)
        assertEquals("/work/app/target/test-classes", m.test.output)
        assertEquals(listOf("pre-compile"), m.tasks.single().hooks)
        assertEquals("g:both", m.classpaths!!.compile.first().key)
        assertEquals("local.jar", m.classpaths!!.test.last().displayName)
        assertNull(m.classpaths!!.test.last().key)
    }

    @Test
    fun `no-deps has no classpaths`() {
        val m = JrsMetadata.parse(json.replace(Regex(""""classpaths": \{.*?\n  \},""", RegexOption.DOT_MATCHES_ALL), "\"classpaths\": null,"))
        assertNull(m.classpaths)
    }

    @Test
    fun `rejects another format version`() {
        assertThrows(IllegalArgumentException::class.java) { JrsMetadata.parse(json.replace("\"version\": 1", "\"version\": 2")) }
        assertThrows(IllegalArgumentException::class.java) { JrsMetadata.parse("not json") }
    }

    @Test
    fun `classpaths map onto scopes`() {
        val libraries = JrsModelApplier.libraries(JrsMetadata.parse(json).classpaths!!)
        assertEquals(
            listOf(
                "g:both:1" to DependencyScope.COMPILE,
                "g:provided:1" to DependencyScope.PROVIDED,
                "g:runtime:1" to DependencyScope.RUNTIME,
                "org.junit:junit-jupiter:5" to DependencyScope.TEST,
                "local.jar" to DependencyScope.TEST,
            ),
            libraries.map { it.entry.displayName to it.scope },
        )
    }
}
