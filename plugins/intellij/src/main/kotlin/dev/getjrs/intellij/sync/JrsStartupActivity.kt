package dev.getjrs.intellij.sync

import com.intellij.notification.NotificationAction
import com.intellij.openapi.project.Project
import com.intellij.openapi.startup.ProjectActivity
import dev.getjrs.intellij.JrsFiles
import dev.getjrs.intellij.JrsNotifications
import dev.getjrs.intellij.settings.JrsSettings

/** Offers to import a project with a `jrs.toml`, and refreshes one already imported. */
class JrsStartupActivity : ProjectActivity {
    override suspend fun execute(project: Project) {
        if (JrsFiles.projectRoot(project) == null) return
        val state = JrsProjectState.getInstance(project)
        when {
            // Imported before: load the model again, which also fills the tool window.
            state.isLinked -> JrsSync.sync(project, quiet = true)
            JrsSettings.getInstance().state.autoImport -> JrsSync.sync(project)
            else -> JrsNotifications.notify(project, "This directory has a ${JrsFiles.MANIFEST}: it is a jrs project.")
                .addAction(NotificationAction.createSimpleExpiring("Load jrs project") { JrsSync.sync(project) })
                .addAction(NotificationAction.createSimpleExpiring("Always load") {
                    JrsSettings.getInstance().state.autoImport = true
                    JrsSync.sync(project)
                })
        }
    }
}
