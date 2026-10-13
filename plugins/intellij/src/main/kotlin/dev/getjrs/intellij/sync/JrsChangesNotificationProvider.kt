package dev.getjrs.intellij.sync

import com.intellij.openapi.fileEditor.FileEditor
import com.intellij.openapi.project.Project
import com.intellij.openapi.vfs.VirtualFile
import com.intellij.ui.EditorNotificationPanel
import com.intellij.ui.EditorNotificationProvider
import dev.getjrs.intellij.JrsFiles
import java.util.function.Function
import javax.swing.JComponent

/** The "Load jrs changes" banner over `jrs.toml` and `jrs.lock`, as Maven and Gradle show one. */
class JrsChangesNotificationProvider : EditorNotificationProvider {
    override fun collectNotificationData(project: Project, file: VirtualFile): Function<in FileEditor, out JComponent?>? {
        if (!JrsFiles.isJrsFile(file)) return null
        val state = JrsProjectState.getInstance(project)
        val text = when {
            JrsFiles.projectRoot(project) == null -> return null
            !state.isLinked -> "This jrs project has not been loaded into the IDE"
            JrsSync.isRunning(project) || !state.isOutOfDate() -> return null
            else -> "jrs.toml or jrs.lock changed since the project was loaded"
        }
        return Function { editor ->
            EditorNotificationPanel(editor, EditorNotificationPanel.Status.Info).apply {
                this.text = text
                createActionLabel(if (state.isLinked) "Load jrs changes" else "Load jrs project") {
                    JrsSync.sync(project)
                }
            }
        }
    }
}
