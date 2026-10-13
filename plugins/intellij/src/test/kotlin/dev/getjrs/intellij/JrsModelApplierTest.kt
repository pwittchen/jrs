package dev.getjrs.intellij

import com.intellij.openapi.roots.CompilerModuleExtension
import com.intellij.openapi.roots.DependencyScope
import com.intellij.openapi.roots.LibraryOrderEntry
import com.intellij.openapi.roots.ModuleRootManager
import com.intellij.openapi.roots.OrderRootType
import com.intellij.testFramework.HeavyPlatformTestCase
import dev.getjrs.intellij.model.JrsMetadata
import dev.getjrs.intellij.sync.JrsModelApplier
import org.jetbrains.jps.model.java.JavaResourceRootType
import org.jetbrains.jps.model.java.JavaSourceRootType
import java.nio.file.Files

/** Applies a model to a real project and reads the module back. */
class JrsModelApplierTest : HeavyPlatformTestCase() {
    fun `test a model becomes one module with roots, outputs and scoped libraries`() {
        val root = createTempDirectory().toPath()
        for (dir in listOf("src/main/java", "src/main/resources", "src/test/java", "target/generated/sources")) {
            Files.createDirectories(root.resolve(dir))
        }
        val jar = Files.createDirectories(root.resolve("cache")).resolve("lib.jar")
        Files.write(jar, byteArrayOf())
        val r = root.toString().replace('\\', '/')
        fun entry(coordinate: String, path: String) = """{ "coordinate": "$coordinate", "jar": "$path", "sources": null }"""
        val json = """
            {
              "version": 1, "jrs": "0.9.0",
              "project": { "name": "app", "version": "1.0.0", "main-class": null, "jrs-version": null,
                           "root": "$r", "manifest": "$r/jrs.toml", "lockfile": "$r/jrs.lock" },
              "jdk": null,
              "java": { "release": 21, "target": null, "encoding": "UTF-8", "javac-args": [] },
              "languages": [ { "name": "java", "version": null, "compiler": null } ],
              "main": { "sources": [ { "language": "java", "dir": "$r/src/main/java" } ],
                        "generated-sources": ["$r/target/generated/sources"],
                        "resources": ["$r/src/main/resources"], "generated-resources": [],
                        "output": "$r/target/classes" },
              "test": { "sources": [ { "language": "java", "dir": "$r/src/test/java" } ],
                        "generated-sources": [], "resources": [], "generated-resources": [],
                        "output": "$r/target/test-classes" },
              "target-dir": "$r/target", "jar": "$r/target/app-1.0.0.jar",
              "classpaths": { "compile": [ ${entry("g:lib:1", "$r/cache/lib.jar")} ],
                              "runtime": [ ${entry("g:lib:1", "$r/cache/lib.jar")} ],
                              "test": [ ${entry("g:lib:1", "$r/cache/lib.jar")} ] },
              "tasks": []
            }
        """.trimIndent()
        val metadata = JrsMetadata.parse(json)
        JrsModelApplier.refreshFiles(metadata)
        val module = JrsModelApplier.apply(project, metadata)
        // A second sync replaces what the first set up rather than adding to it.
        JrsModelApplier.apply(project, metadata)

        assertEquals("app", module.name)
        val roots = ModuleRootManager.getInstance(module)
        val folders = roots.contentEntries.single().sourceFolders
        fun type(dir: String) = folders.single { it.url.endsWith(dir) }.rootType
        assertEquals(JavaSourceRootType.SOURCE, type("src/main/java"))
        assertEquals(JavaSourceRootType.TEST_SOURCE, type("src/test/java"))
        assertEquals(JavaResourceRootType.RESOURCE, type("src/main/resources"))
        assertEquals(JavaSourceRootType.SOURCE, type("target/generated/sources"))
        assertTrue(roots.contentEntries.single().excludeFolderUrls.single().endsWith("/target"))

        val output = roots.getModuleExtension(CompilerModuleExtension::class.java)
        assertTrue(output.compilerOutputUrl!!.endsWith("target/classes"))
        assertTrue(output.compilerOutputUrlForTests!!.endsWith("target/test-classes"))

        val library = roots.orderEntries.filterIsInstance<LibraryOrderEntry>().single()
        assertEquals("jrs: g:lib:1", library.libraryName)
        assertEquals(DependencyScope.COMPILE, library.scope)
        assertTrue(library.getRootUrls(OrderRootType.CLASSES).single().endsWith("lib.jar!/"))
    }
}
