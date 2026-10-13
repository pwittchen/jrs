package dev.getjrs.intellij.actions

import com.intellij.execution.ExecutionException
import com.intellij.openapi.actionSystem.ActionUpdateThread
import com.intellij.openapi.actionSystem.AnActionEvent
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.progress.ProgressIndicator
import com.intellij.openapi.progress.Task
import com.intellij.openapi.project.DumbAwareAction
import com.intellij.openapi.project.Project
import com.intellij.openapi.ui.DialogWrapper
import com.intellij.openapi.vfs.LocalFileSystem
import com.intellij.ui.components.JBScrollPane
import com.intellij.ui.components.JBTextArea
import com.intellij.util.ui.JBUI
import dev.getjrs.intellij.JrsCli
import dev.getjrs.intellij.JrsFiles
import dev.getjrs.intellij.JrsNotifications
import dev.getjrs.intellij.sync.JrsSync
import java.nio.file.Files
import java.nio.file.Path
import javax.swing.JComponent

/** Runs `jrs migrate` on a Maven or Gradle project, shows its report and loads the result. */
class JrsMigrateAction : DumbAwareAction() {
    override fun getActionUpdateThread(): ActionUpdateThread = ActionUpdateThread.BGT

    override fun update(e: AnActionEvent) {
        val root = e.project?.basePath?.let(Path::of)
        e.presentation.isEnabledAndVisible = root != null &&
            !Files.exists(root.resolve(JrsFiles.MANIFEST)) &&
            BUILD_FILES.any { Files.isRegularFile(root.resolve(it)) }
    }

    override fun actionPerformed(e: AnActionEvent) {
        val project = e.project ?: return
        val root = Path.of(project.basePath ?: return)
        object : Task.Backgroundable(project, "jrs migrate", true) {
            override fun run(indicator: ProgressIndicator) {
                val output = try {
                    JrsCli.run(root, listOf("migrate", "--path", root.toString()), indicator)
                } catch (e: ExecutionException) {
                    JrsNotifications.error(project, "Could not run ${JrsCli.executable()}: ${e.message}")
                    return
                }
                LocalFileSystem.getInstance().refreshAndFindFileByNioFile(root.resolve(JrsFiles.MANIFEST))
                val report = (output.stdout + output.stderr).trim()
                ApplicationManager.getApplication().invokeLater {
                    ReportDialog(project, report, output.exitCode == 0).show()
                    if (output.exitCode == 0) JrsSync.sync(project)
                }
            }
        }.queue()
    }

    private class ReportDialog(project: Project, private val report: String, ok: Boolean) : DialogWrapper(project) {
        init {
            title = if (ok) "jrs migrate: jrs.toml written" else "jrs migrate failed"
            setOKButtonText("Close")
            init()
        }

        override fun createActions() = arrayOf(okAction)

        override fun createCenterPanel(): JComponent = JBScrollPane(
            JBTextArea(report).apply {
                isEditable = false
                font = JBUI.Fonts.create(java.awt.Font.MONOSPACED, font.size)
            },
        ).apply { preferredSize = JBUI.size(760, 480) }
    }

    companion object {
        private val BUILD_FILES = listOf("pom.xml", "build.gradle", "build.gradle.kts")
    }
}
