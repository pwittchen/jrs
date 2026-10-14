package dev.getjrs.intellij.sync

import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.project.Project
import com.intellij.openapi.vfs.newvfs.BulkFileListener
import com.intellij.openapi.vfs.newvfs.events.VFileEvent
import com.intellij.ui.EditorNotifications
import dev.getjrs.intellij.JrsFiles
import dev.getjrs.intellij.settings.JrsSettings

/** Notices a change to `jrs.toml` or `jrs.lock` and either reloads or shows the "Load jrs changes" banner. */
class JrsManifestChangeListener(private val project: Project) : BulkFileListener {
    override fun after(events: List<VFileEvent>) {
        val root = JrsFiles.projectRoot(project)?.toString() ?: return
        val state = JrsProjectState.getInstance(project)
        // A workspace member's own jrs.toml counts too: its lockfile is the root's.
        val memberRoots = state.state.memberRoots.map { it.replace('\\', '/') }.toSet()
        val roots = setOfNotNull(root.replace('\\', '/'), state.state.workspaceRoot?.replace('\\', '/'))
        val touched = events.any { event ->
            val name = event.path.substringAfterLast('/')
            val dir = event.path.substringBeforeLast('/')
            ((name == JrsFiles.MANIFEST || name == JrsFiles.LOCKFILE) && dir in roots) ||
                (name == JrsFiles.MANIFEST && dir in memberRoots)
        }
        if (!touched) return
        if (!state.isLinked || JrsSync.isRunning(project)) return
        ApplicationManager.getApplication().executeOnPooledThread {
            if (project.isDisposed || !state.isOutOfDate()) return@executeOnPooledThread
            if (JrsSettings.getInstance().state.autoReload) {
                JrsSync.sync(project, quiet = true)
            } else {
                EditorNotifications.getInstance(project).updateAllNotifications()
            }
        }
    }
}
