package dev.getjrs.intellij

import dev.getjrs.intellij.manifest.JrsManifestSchema
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class JrsManifestSchemaTest {
    private val manifest = """
        # comment = not a key
        [project]
        name = "app"
        versoin = "1.0.0"
        main-class = "com.example.Main"

        [java]
        javac-args = [
          "-Xlint:all",   # still = inside the array
          "-parameters",
        ]
        jdk = 21

        [dependencies]
        "com.google.guava:guava" = "33.0.0-jre"
        "org.example:lib" = { version = "1.0", compile-only = true }

        [kotlin]
        version = "2.4.20"
        plugins = ["spring"]
        kotlinc-args = ""${'"'}
        bogus = "inside a multi-line string"
        ""${'"'}

        [tasks.gen]
        run = ["echo"]
        output = "x"

        [test.suites.it]
        forks = 2

        [hooks]
        pre-compile = ["gen"]
        post-everything = ["gen"]

        [toolz]
        x = 1
    """.trimIndent()

    @Test
    fun `reports unknown keys and tables only`() {
        val problems = JrsManifestSchema.problems(manifest)
        assertEquals(
            listOf(
                "unknown key `project.versoin` in jrs.toml (ignored)",
                "unknown key `tasks.gen.output` in jrs.toml (ignored)",
                "unknown key `hooks.post-everything` in jrs.toml (ignored)",
                "unknown table `toolz` in jrs.toml (ignored)",
            ),
            problems.map { it.message },
        )
        val first = problems.first()
        assertEquals("versoin", manifest.substring(first.start, first.end))
    }

    @Test
    fun `completes keys of the current table and table names`() {
        val before = "[project]\nname = \"app\"\n"
        val keys = JrsManifestSchema.completion(before, "ma")!!
        assertEquals("ma", keys.prefix)
        assertEquals(true, "main-class" in keys.candidates)

        val tables = JrsManifestSchema.completion(before, "[ko")!!
        assertEquals("ko", tables.prefix)
        assertEquals(true, "kotlin" in tables.candidates)

        assertNull(JrsManifestSchema.completion(before, "name = \"a"))
        assertNull(JrsManifestSchema.completion("[dependencies]\n", "com"))
    }

    @Test
    fun `splits dotted and quoted keys`() {
        assertEquals(listOf("tasks", "a.b", "env"), JrsManifestSchema.splitKey(" tasks.\"a.b\".env "))
    }
}
