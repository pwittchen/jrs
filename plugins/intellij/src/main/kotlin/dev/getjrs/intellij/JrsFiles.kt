package dev.getjrs.intellij

import com.intellij.openapi.project.Project
import com.intellij.openapi.vfs.VirtualFile
import java.nio.file.Files
import java.nio.file.Path

/** The two files that make a directory a jrs project. */
object JrsFiles {
    const val MANIFEST = "jrs.toml"
    const val LOCKFILE = "jrs.lock"

    fun isJrsFile(file: VirtualFile): Boolean = file.name == MANIFEST || file.name == LOCKFILE

    fun isManifest(file: VirtualFile): Boolean = file.name == MANIFEST

    /** The project's root directory when it holds a `jrs.toml`, else null. */
    fun projectRoot(project: Project): Path? {
        val base = project.basePath ?: return null
        val root = Path.of(base)
        return root.takeIf { Files.isRegularFile(it.resolve(MANIFEST)) }
    }
}
