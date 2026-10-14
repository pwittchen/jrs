package dev.getjrs.intellij.sync

import com.intellij.openapi.application.WriteAction
import com.intellij.openapi.module.JavaModuleType
import com.intellij.openapi.module.Module
import com.intellij.openapi.module.ModuleManager
import com.intellij.openapi.module.ModuleType
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
import com.intellij.openapi.roots.ModuleOrderEntry
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

/**
 * Turns a [JrsMetadata] into one IntelliJ module, the project JDK and the
 * module's libraries; a workspace's models into one module per member, each
 * depending on the members it depends on.
 */
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

    /** The IntelliJ scope of a member's classes on a dependent's classpath. */
    fun scope(classpath: String): DependencyScope = when (classpath) {
        "provided" -> DependencyScope.PROVIDED
        "runtime" -> DependencyScope.RUNTIME
        "test" -> DependencyScope.TEST
        else -> DependencyScope.COMPILE
    }

    /**
     * Applies a workspace's models, one module per member, then each member's
     * dependencies on the others; must run on the EDT. Returns the modules.
     */
    fun applyAll(project: Project, members: List<JrsMetadata>): List<Module> {
        if (members.size == 1 && members[0].modules.isEmpty()) return listOf(apply(project, members[0]))
        val modules = members.map { apply(project, it, byName = true) }
        val byName = members.zip(modules).associate { (m, module) -> m.project.name to module }
        WriteAction.run<RuntimeException> { clearOthers(project, members, modules) }
        WriteAction.run<RuntimeException> {
            for ((metadata, module) in members.zip(modules)) {
                val model = ModuleRootManager.getInstance(module).modifiableModel
                try {
                    model.orderEntries
                        .filterIsInstance<ModuleOrderEntry>()
                        .filter { it.moduleName in byName }
                        .forEach(model::removeOrderEntry)
                    // Every member it reaches, not only those it names: jrs puts
                    // them all on its classpaths, nearest first.
                    for (ref in metadata.modules) {
                        val upstream = byName[ref.name] ?: continue
                        model.addModuleOrderEntry(upstream).scope = scope(ref.classpath)
                    }
                    model.commit()
                } catch (e: RuntimeException) {
                    model.dispose()
                    throw e
                }
            }
        }
        return modules
    }

    /**
     * The modules beside a workspace's members. IntelliJ sets a folder up
     * before the plugin runs: one module over the whole tree, or a module per
     * source folder it detects (`app/src/main` as `main3`). Either claims the
     * members' sources, and since a file belongs to the module whose content
     * root is nearest, a member's file would land in a module with none of its
     * libraries or module dependencies. So, for every module that is not a
     * member: a content root inside a member's directory is removed, and a
     * module left with none goes; a content root above the members stays as
     * the workspace's root, without source folders, jrs libraries or module
     * dependencies. A module a previous sync made for a member that is no
     * longer one goes too. Modules elsewhere are the user's, and stay as they
     * are.
     */
    private fun clearOthers(project: Project, members: List<JrsMetadata>, modules: List<Module>) {
        val manager = ModuleManager.getInstance(project)
        val state = JrsProjectState.getInstance(project).state
        val memberRoots = members.map { FileUtil.toSystemIndependentName(it.project.root) }
        val previous = state.memberModules.toSet()
        val gone = mutableListOf<Module>()
        for (other in manager.modules.filter { it !in modules }) {
            if (other.name in previous) {
                gone += other
                continue
            }
            val roots = ModuleRootManager.getInstance(other).contentEntries.map {
                it to VfsUtilCore.urlToPath(it.url)
            }
            val inside = roots.filter { (_, path) -> memberRoots.any { FileUtil.isAncestor(it, path, false) } }
            val above = roots.filter { (_, path) -> memberRoots.any { FileUtil.isAncestor(path, it, true) } }
            if (inside.isEmpty() && above.isEmpty()) continue
            if (inside.size == roots.size) {
                gone += other
                continue
            }
            val model = ModuleRootManager.getInstance(other).modifiableModel
            try {
                val stale = (inside + above).map { (entry, _) -> entry.url }.toSet()
                model.contentEntries.filter { it.url in stale }.forEach(model::removeContentEntry)
                // What stays above the members is the workspace's own directory.
                for ((_, path) in above) model.addContentEntry(url(path)).addExcludeFolder(url("$path/target"))
                model.orderEntries
                    .filter {
                        it is ModuleOrderEntry ||
                            (it is LibraryOrderEntry && it.libraryName?.startsWith(LIBRARY_PREFIX) == true)
                    }
                    .forEach(model::removeOrderEntry)
                model.commit()
            } catch (e: RuntimeException) {
                model.dispose()
                throw e
            }
        }
        if (gone.isNotEmpty()) {
            val model = manager.getModifiableModel()
            gone.forEach(model::disposeModule)
            model.commit()
        }
    }

    /** Applies the model; must run on the EDT. Returns the module it set up. */
    fun apply(project: Project, metadata: JrsMetadata): Module = apply(project, metadata, byName = false)

    private fun apply(project: Project, metadata: JrsMetadata, byName: Boolean): Module =
        WriteAction.compute<Module, RuntimeException> {
        val sdk = metadata.jdk?.home?.let { findOrCreateSdk(it, metadata.jdk.version) }
        if (sdk != null) ProjectRootManager.getInstance(project).projectSdk = sdk
        val release = metadata.java.release ?: metadata.jdk?.version
        release?.let { LanguageLevel.parse(it.toString()) }?.let {
            LanguageLevelProjectExtension.getInstance(project).languageLevel = it
        }

        val module = findOrCreateModule(project, metadata, byName)
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

    /**
     * The module for [metadata]: by the name the last sync gave it, by the
     * project's name, or by its content root. A workspace member is never
     * found by the last sync's single module name, which another member may own.
     */
    private fun findOrCreateModule(project: Project, metadata: JrsMetadata, byName: Boolean): Module {
        val manager = ModuleManager.getInstance(project)
        val state = JrsProjectState.getInstance(project).state
        val rootUrl = url(metadata.project.root)
        val existing = state.moduleName?.takeUnless { byName }?.let(manager::findModuleByName)
            ?: manager.findModuleByName(metadata.project.name)
            ?: manager.modules.firstOrNull { m ->
                ModuleRootManager.getInstance(m).contentRootUrls.any { it == rootUrl }
            }
        val javaModule = JavaModuleType.getModuleType().id
        if (existing != null && ModuleType.get(existing).id == javaModule) return existing

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
