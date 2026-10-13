package dev.getjrs.intellij

import com.intellij.ide.FileIconProvider
import com.intellij.openapi.project.Project
import com.intellij.openapi.vfs.VirtualFile
import javax.swing.Icon

/** Shows the jrs logo on `jrs.toml` and `jrs.lock`, as Maven and Gradle do on their build files. */
class JrsFileIconProvider : FileIconProvider {
    override fun getIcon(file: VirtualFile, flags: Int, project: Project?): Icon? =
        if (!file.isDirectory && JrsFiles.isJrsFile(file)) JrsIcons.Jrs else null
}
