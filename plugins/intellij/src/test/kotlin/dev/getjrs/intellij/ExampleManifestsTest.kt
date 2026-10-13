package dev.getjrs.intellij

import dev.getjrs.intellij.manifest.JrsManifestSchema
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.File

/** The repository's own manifests are valid, so the inspection must find nothing in them. */
class ExampleManifestsTest {
    @Test
    fun `the example manifests have no unknown keys`() {
        val repo = File("../..").canonicalFile
        val manifests = listOf("examples", "tests/fixtures")
            .map { File(repo, it) }
            .filter { it.isDirectory }
            .flatMap { dir -> dir.walk().filter { it.name == "jrs.toml" }.toList() }
        assertTrue("no jrs.toml found under $repo", manifests.isNotEmpty())
        for (manifest in manifests) {
            assertEquals(manifest.path, emptyList<String>(), JrsManifestSchema.problems(manifest.readText()).map { it.message })
        }
    }
}
