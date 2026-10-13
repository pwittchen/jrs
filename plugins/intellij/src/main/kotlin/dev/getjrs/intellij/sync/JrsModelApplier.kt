package dev.getjrs.intellij.sync

import com.intellij.openapi.application.WriteAction
import com.intellij.openapi.module.JavaModuleType
import com.intellij.openapi.module.Module
import com.intellij.openapi.module.ModuleManager
import com.intellij.openapi.project.Project
import com.intellij.openapi.projectRoots.JavaSdk
import com.intellij.openapi.projectRoots.ProjectJdkTable
import com.intellij.openapi.projectRoots.Sdk
import com.intellij.openapi.roots.CompilerModuleExtension
import com.intellij.openapi.roots.ContentEntry
import com.intellij.openapi.roots.DependencyScope
import com.intellij.openapi.roots.LanguageLevelProjectExtension
import com.intellij.openapi.roots.LibraryOrderEntry
import com.intellij.openapi.roots.ModifiableRootModel
import com.intellij.openapi.roots.ModuleRootManager
import com.intellij.openapi.roots.OrderRootType
import com.intellij.openapi.roots.ProjectRootManager
import com.intellij.openapi.util.io.FileUtil
import com.intellij.openapi.vfs.JarFileSystem
import com.intellij.openapi.vfs.LocalFileSystem
import com.intellij.openapi.vfs.VfsUtilCore
import com.intellij.openapi.vfs.VirtualFileManager
import com.intellij.pom.java.LanguageLevel
import dev.getjrs.intellij.model.JrsMetadata
import org.jetbrains.jps.model.java.JavaResourceRootType
import org.jetbrains.jps.model.java.JavaSourceRootType
import org.jetbrains.jps.model.java.JpsJavaExtensionService
import java.nio.file.Path

/** Turns a [JrsMetadata] into one IntelliJ module, the project JDK and the module's libraries. */
object JrsModelApplier {
    /** Prefix of the module libraries a sync owns, so the next sync can replace them. */
    const val LIBRARY_PREFIX = "jrs: "

    /** One library per jar, with the IntelliJ scope its classpaths imply. */
    data class Library(val entry: JrsMetadata.Entry, val scope: DependencyScope)

    /**
     * Maps the three classpaths onto scopes: on both the compile and runtime
     * classpaths is COMPILE, compile-only is PROVIDED, runtime-only is
     * RUNTIME and test-only is TEST. Order follows the build's: the compile
     * classpath first, then what only the runtime and test classpaths add.
     */
    fun libraries(classpaths: JrsMetadata.Classpaths): List<Library> {
        val compile = classpaths.compile.map { it.jar }.toSet()
        val runtime = classpaths.runtime.map { it.jar }.toSet()
        val seen = HashSet<String>()
        val out = ArrayList<Library>()
        for (entry in classpaths.compile + classpaths.runtime + classpaths.test) {
            if (!seen.add(entry.jar)) continue
            val scope = when {
                entry.jar in compile && entry.jar in runtime -> DependencyScope.COMPILE
                entry.jar in compile -> DependencyScope.PROVIDED
                entry.jar in runtime -> DependencyScope.RUNTIME
                else -> DependencyScope.TEST
            }
            out += Library(entry, scope)
        }
        return out
    }

    /** Registers every directory and jar in the VFS; call off the EDT, before [apply]. */
    fun refreshFiles(metadata: JrsMetadata) {
        val fs = LocalFileSystem.getInstance()
        val paths = buildList {
            for (unit in listOf(metadata.main, metadata.test)) {
                unit.sources.forEach { add(it.dir) }
                addAll(unit.generatedSources)
                addAll(unit.resources)
                addAll(unit.generatedResources)
            }
            metadata.classpaths?.let { cp ->
                (cp.compile + cp.runtime + cp.test).forEach { e ->
                    add(e.jar)
                    e.sources?.let(::add)
                }
            }
        }
        for (path in paths) fs.refreshAndFindFileByNioFile(Path.of(path))
    }

    /** Applies the model; must run on the EDT. Returns the module it set up. */
    fun apply(project: Project, metadata: JrsMetadata): Module = WriteAction.compute<Module, RuntimeException> {
        val sdk = metadata.jdk?.home?.let { findOrCreateSdk(it, metadata.jdk.version) }
        if (sdk != null) ProjectRootManager.getInstance(project).projectSdk = sdk
        val release = metadata.java.release ?: metadata.jdk?.version
        release?.let { LanguageLevel.parse(it.toString()) }?.let {
            LanguageLevelProjectExtension.getInstance(project).languageLevel = it
        }

        val module = findOrCreateModule(project, metadata)
        val model = ModuleRootManager.getInstance(module).modifiableModel
        try {
            configure(model, metadata)
            model.commit()
        } catch (e: RuntimeException) {
            model.dispose()
            throw e
        }
        module
    }

    private fun findOrCreateModule(project: Project, metadata: JrsMetadata): Module {
        val manager = ModuleManager.getInstance(project)
        val state = JrsProjectState.getInstance(project).state
        val rootUrl = url(metadata.project.root)
        val existing = state.moduleName?.let(manager::findModuleByName)
            ?: manager.findModuleByName(metadata.project.name)
            ?: manager.modules.firstOrNull { m ->
                ModuleRootManager.getInstance(m).contentRootUrls.any { it == rootUrl }
            }
        val javaModule = JavaModuleType.getModuleType().id
        if (existing != null && existing.moduleTypeName == javaModule) return existing

        val modules = manager.getModifiableModel()
        existing?.let(modules::disposeModule)
        val base = project.basePath ?: metadata.project.root
        val module = modules.newModule(Path.of(base, ".idea", "${metadata.project.name}.iml"), javaModule)
        modules.commit()
        return module
    }

    private fun configure(model: ModifiableRootModel, metadata: JrsMetadata) {
        model.contentEntries.forEach(model::removeContentEntry)
        model.inheritSdk()

        val root = metadata.project.root
        val content = model.addContentEntry(url(root))
        fun entryFor(dir: String): ContentEntry =
            if (FileUtil.isAncestor(root, dir, false)) {
                content
            } else {
                model.contentEntries.firstOrNull { FileUtil.isAncestor(VfsUtilCore.urlToPath(it.url), dir, false) }
                    ?: model.addContentEntry(url(dir))
            }

        val generated = JpsJavaExtensionService.getInstance().createSourceRootProperties("", true)
        for ((unit, test) in listOf(metadata.main to false, metadata.test to true)) {
            val sourceType = if (test) JavaSourceRootType.TEST_SOURCE else JavaSourceRootType.SOURCE
            val resourceType = if (test) JavaResourceRootType.TEST_RESOURCE else JavaResourceRootType.RESOURCE
            unit.sources.forEach { entryFor(it.dir).addSourceFolder(url(it.dir), sourceType) }
            unit.generatedSources.forEach { entryFor(it).addSourceFolder(url(it), sourceType, generated) }
            (unit.resources + unit.generatedResources).forEach { entryFor(it).addSourceFolder(url(it), resourceType) }
        }
        entryFor(metadata.targetDir).addExcludeFolder(url(metadata.targetDir))

        model.getModuleExtension(CompilerModuleExtension::class.java)?.apply {
            inheritCompilerOutputPath(false)
            setCompilerOutputPath(url(metadata.main.output))
            setCompilerOutputPathForTests(url(metadata.test.output))
            setExcludeOutput(true)
        }

        // `--no-deps` carries no classpaths: keep the libraries the last full sync set up.
        val classpaths = metadata.classpaths ?: return
        val table = model.moduleLibraryTable
        table.libraries.filter { it.name?.startsWith(LIBRARY_PREFIX) == true }.forEach(table::removeLibrary)
        for (library in libraries(classpaths)) {
            val created = table.createLibrary(LIBRARY_PREFIX + library.entry.displayName)
            val libraryModel = created.modifiableModel
            libraryModel.addRoot(jarUrl(library.entry.jar), OrderRootType.CLASSES)
            library.entry.sources?.let { libraryModel.addRoot(jarUrl(it), OrderRootType.SOURCES) }
            libraryModel.commit()
            (model.findLibraryOrderEntry(created) as? LibraryOrderEntry)?.scope = library.scope
        }
    }

    private fun findOrCreateSdk(home: String, version: Int): Sdk? {
        val table = ProjectJdkTable.getInstance()
        val javaSdk = JavaSdk.getInstance()
        val normalized = FileUtil.toSystemIndependentName(home)
        table.getSdksOfType(javaSdk)
            .firstOrNull { it.homePath?.let(FileUtil::toSystemIndependentName) == normalized }
            ?.let { return it }
        if (!javaSdk.isValidSdkHome(home)) return null
        val taken = table.allJdks.map { it.name }.toSet()
        val name = generateSequence(1) { it + 1 }
            .map { if (it == 1) "jrs JDK $version" else "jrs JDK $version ($it)" }
            .first { it !in taken }
        val sdk = javaSdk.createJdk(name, home, false)
        table.addJdk(sdk)
        return sdk
    }

    private fun url(path: String): String = VfsUtilCore.pathToUrl(FileUtil.toSystemIndependentName(path))

    private fun jarUrl(path: String): String =
        VirtualFileManager.constructUrl(
            JarFileSystem.PROTOCOL,
            FileUtil.toSystemIndependentName(path) + JarFileSystem.JAR_SEPARATOR,
        )
}
