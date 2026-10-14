package dev.getjrs.intellij

import com.intellij.openapi.application.WriteAction
import com.intellij.openapi.module.Module
import com.intellij.openapi.module.ModuleUtilCore
import com.intellij.openapi.roots.ModuleRootManager
import com.intellij.openapi.vfs.LocalFileSystem
import com.intellij.openapi.vfs.VfsUtil
import com.intellij.openapi.vfs.VfsUtilCore
import com.intellij.psi.PsiJavaFile
import com.intellij.psi.PsiManager
import com.intellij.testFramework.HeavyPlatformTestCase
import com.intellij.testFramework.IndexingTestUtil
import dev.getjrs.intellij.model.JrsMetadata
import dev.getjrs.intellij.sync.JrsModelApplier
import org.jetbrains.jps.model.java.JavaSourceRootType
import java.nio.file.Files
import java.nio.file.Path
import java.util.jar.JarEntry
import java.util.jar.JarOutputStream

/**
 * A workspace of three members imported into a project the IDE opened as a
 * plain folder, with a module of its own claiming every `src/` directory —
 * what IntelliJ sets up before the plugin runs. After the import every
 * source file must belong to its member's module and resolve every import:
 * the members it depends on, and the jars its classpaths hold.
 */
class JrsWorkspaceImportTest : HeavyPlatformTestCase() {
    private lateinit var root: Path

    private fun write(path: String, text: String): Path {
        val file = root.resolve(path)
        Files.createDirectories(file.parent)
        Files.writeString(file, text)
        return file
    }

    /**
     * A jar holding one empty public class, its class file written by hand:
     * the IDE's test JVM has no compiler, and resolving an import needs only
     * a class file that reads.
     */
    private fun jar(name: String, className: String): Path {
        val internal = className.replace('.', '/')
        val bytes = java.io.ByteArrayOutputStream()
        java.io.DataOutputStream(bytes).use { out ->
            out.writeInt(0xCAFEBABE.toInt())
            out.writeShort(0) // minor
            out.writeShort(52) // Java 8
            out.writeShort(5) // constant pool count: four entries
            out.writeByte(7); out.writeShort(2) // #1 Class #2
            out.writeByte(1); out.writeUTF(internal) // #2
            out.writeByte(7); out.writeShort(4) // #3 Class #4
            out.writeByte(1); out.writeUTF("java/lang/Object") // #4
            out.writeShort(0x0021) // public super
            out.writeShort(1) // this
            out.writeShort(3) // super
            repeat(4) { out.writeShort(0) } // interfaces, fields, methods, attributes
        }
        val jar = root.resolve("cache/$name.jar")
        Files.createDirectories(jar.parent)
        JarOutputStream(Files.newOutputStream(jar)).use { zip ->
            zip.putNextEntry(JarEntry("$internal.class"))
            zip.write(bytes.toByteArray())
            zip.closeEntry()
        }
        return jar
    }

    private fun member(name: String, classpaths: String, modules: String): String {
        val r = root.toString().replace('\\', '/')
        return """
            {
              "version": 1, "jrs": "0.9.0",
              "project": { "name": "$name", "version": "1.0.0", "main-class": null, "jrs-version": null,
                           "root": "$r/$name", "manifest": "$r/$name/jrs.toml", "lockfile": "$r/jrs.lock" },
              "jdk": null,
              "java": { "release": 17, "target": null, "encoding": "UTF-8", "javac-args": [] },
              "languages": [ { "name": "java", "version": null, "compiler": null } ],
              "main": { "sources": [ { "language": "java", "dir": "$r/$name/src/main/java" } ],
                        "generated-sources": [], "resources": [], "generated-resources": [],
                        "output": "$r/$name/target/classes" },
              "test": { "sources": [ { "language": "java", "dir": "$r/$name/src/test/java" } ],
                        "generated-sources": [], "resources": [], "generated-resources": [],
                        "output": "$r/$name/target/test-classes" },
              "target-dir": "$r/$name/target", "jar": "$r/$name/target/$name-1.0.0.jar",
              "classpaths": $classpaths,
              "tasks": [], "workspace": "$r", "modules": $modules
            }
        """.trimIndent()
    }

    fun `test every file of a workspace resolves after the import`() = importAndCheck(perSourceFolder = false)

    /**
     * IntelliJ's other set-up of a folder: a module per source folder it
     * detects, `core/src/main` as `main`, each with its own content root
     * inside the member's.
     */
    fun `test the modules IntelliJ made per source folder give the members their files back`() =
        importAndCheck(perSourceFolder = true)

    private fun importAndCheck(perSourceFolder: Boolean) {
        root = createTempDirectory().toPath().toRealPath()
        val r = root.toString().replace('\\', '/')
        write("jrs.toml", "[workspace]\nmembers = [\"app\", \"journal\", \"core\"]\n")
        for (m in listOf("core", "journal", "app")) {
            write("$m/jrs.toml", "[project]\nname = \"$m\"\nversion = \"1.0.0\"\n")
            Files.createDirectories(root.resolve("$m/src/test/java"))
        }
        val lib = jar("lib", "lib.Lib")
        val testLib = jar("testlib", "testlib.Check")
        val core = write("core/src/main/java/demo/core/Core.java", "package demo.core;\npublic class Core { }\n")
        val journal = write(
            "journal/src/main/java/demo/journal/Journal.java",
            "package demo.journal;\nimport demo.core.Core;\n" +
                "import lib.Lib;\n" +
                "public class Journal { Core core; }\n",
        )
        val app = write(
            "app/src/main/java/demo/app/App.java",
            "package demo.app;\nimport demo.core.Core;\nimport demo.journal.Journal;\n" +
                "import lib.Lib;\n" +
                "public class App { Core core; Journal journal; }\n",
        )
        val appTest = write(
            "app/src/test/java/demo/app/AppTest.java",
            "package demo.app;\nimport demo.core.Core;\n" +
                "import testlib.Check;\n" +
                "class AppTest { App app; Core core; }\n",
        )

        // What IntelliJ sets up when it opens the folder: one module over the
        // whole tree, every source directory its own; or a module per source
        // folder, the way its source-root detection names them.
        val fs = LocalFileSystem.getInstance()
        val rootDir = fs.refreshAndFindFileByNioFile(root)!!
        VfsUtil.markDirtyAndRefresh(false, true, true, rootDir)
        val detected = mutableListOf<String>()
        WriteAction.run<RuntimeException> {
            if (!perSourceFolder) {
                val model = ModuleRootManager.getInstance(module).modifiableModel
                val content = model.addContentEntry(rootDir)
                for (m in listOf("core", "journal", "app")) {
                    content.addSourceFolder(fs.findFileByNioFile(root.resolve("$m/src/main/java"))!!, false)
                    content.addSourceFolder(fs.findFileByNioFile(root.resolve("$m/src/test/java"))!!, true)
                }
                model.commit()
            } else {
                val manager = com.intellij.openapi.module.ModuleManager.getInstance(project)
                val modules = manager.getModifiableModel()
                val made = mutableListOf<Pair<Module, Path>>()
                var n = 0
                for (m in listOf("core", "journal", "app")) for (unit in listOf("main", "test")) {
                    val dir = root.resolve("$m/src/$unit")
                    val name = if (n < 2) unit else "$unit${n / 2 + 1}"
                    n++
                    detected += name
                    made += modules.newModule(dir.resolve("$name.iml"), com.intellij.openapi.module.JavaModuleType.getModuleType().id) to dir
                }
                modules.commit()
                for ((detectedModule, dir) in made) {
                    val model = ModuleRootManager.getInstance(detectedModule).modifiableModel
                    val content = model.addContentEntry(fs.refreshAndFindFileByNioFile(dir)!!)
                    content.addSourceFolder(fs.findFileByNioFile(dir.resolve("java"))!!, dir.fileName.toString() == "test")
                    model.commit()
                }
            }
        }

        fun entry(jar: Path) = """{ "coordinate": "g:${jar.fileName}:1", "jar": "$jar", "sources": null }"""
        val libEntries = entry(lib)
        val testEntries = "${entry(lib)}, ${entry(testLib)}"
        fun ref(name: String, direct: Boolean) =
            """{ "name": "$name", "version": "1.0.0", "root": "$r/$name", "output": "$r/$name/target/classes",
                 "classpath": "compile", "direct": $direct }"""
        val members = JrsMetadata.parseAll(
            """{ "version": 1, "jrs": "0.9.0",
                 "workspace": { "name": "ws", "root": "$r", "manifest": "$r/jrs.toml", "lockfile": "$r/jrs.lock" },
                 "members": [
                   ${member("app", """{ "compile": [ $libEntries ], "runtime": [ $libEntries ], "test": [ $testEntries ] }""",
                "[ ${ref("journal", true)}, ${ref("core", false)} ]")},
                   ${member("journal", """{ "compile": [ $libEntries ], "runtime": [ $libEntries ], "test": [ $libEntries ] }""",
                "[ ${ref("core", true)} ]")},
                   ${member("core", """{ "compile": [], "runtime": [], "test": [] }""", "[]")}
                 ] }""",
        )
        members.forEach(JrsModelApplier::refreshFiles)
        val modules = JrsModelApplier.applyAll(project, members)
        IndexingTestUtil.waitUntilIndexesAreReady(project)
        val byName = modules.associateBy { it.name }

        // No other module claims a member's sources.
        val stray = ModuleRootManager.getInstance(module).sourceRoots
        assertEquals(emptyList<String>(), stray.map { it.path })
        val left = com.intellij.openapi.module.ModuleManager.getInstance(project).modules.map { it.name }
        assertEquals(emptyList<String>(), detected.filter { it in left })

        val expected = mapOf(core to "core", journal to "journal", app to "app", appTest to "app")
        for ((path, owner) in expected) {
            val file = fs.refreshAndFindFileByNioFile(path)!!
            val found: Module? = ModuleUtilCore.findModuleForFile(file, project)
            assertEquals(path.toString(), owner, found?.name)
            val psi = PsiManager.getInstance(project).findFile(file) as PsiJavaFile
            for (statement in psi.importList!!.allImportStatements) {
                assertNotNull(
                    "${VfsUtilCore.virtualToIoFile(file).name}: ${statement.text} does not resolve",
                    statement.resolve(),
                )
            }
        }
        // Test sources are test sources of their member.
        val appRoots = ModuleRootManager.getInstance(byName.getValue("app")).contentEntries.flatMap { it.sourceFolders.toList() }
        assertTrue(appRoots.any { it.rootType == JavaSourceRootType.TEST_SOURCE })
    }
}
