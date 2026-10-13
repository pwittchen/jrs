package dev.getjrs.intellij.actions

import com.intellij.execution.ExecutionException
import com.intellij.openapi.actionSystem.AnActionEvent
import com.intellij.openapi.actionSystem.DataKey
import com.intellij.openapi.progress.ProgressIndicator
import com.intellij.openapi.progress.Task
import com.intellij.openapi.project.Project
import com.intellij.openapi.ui.ComboBox
import com.intellij.openapi.ui.DialogWrapper
import com.intellij.openapi.ui.Messages
import com.intellij.openapi.vfs.LocalFileSystem
import com.intellij.ui.components.JBTextField
import com.intellij.util.ui.FormBuilder
import dev.getjrs.intellij.JrsCli
import dev.getjrs.intellij.JrsFiles
import dev.getjrs.intellij.JrsNotifications
import dev.getjrs.intellij.sync.JrsSync
import javax.swing.JComponent

/** The dependency selected in the jrs tool window, as `group:artifact`. */
val SELECTED_DEPENDENCY: DataKey<String> = DataKey.create("jrs.selectedDependency")

/**
 * Runs `jrs add` or `jrs remove` and reloads the project, so `edit.rs` stays
 * the one place that rewrites `jrs.toml`.
 */
internal fun editManifest(project: Project, title: String, args: List<String>) {
    val root = JrsFiles.projectRoot(project) ?: return
    object : Task.Backgroundable(project, title, true) {
        override fun run(indicator: ProgressIndicator) {
            val output = try {
                JrsCli.run(root, args, indicator)
            } catch (e: ExecutionException) {
                JrsNotifications.error(project, "Could not run ${JrsCli.executable()}: ${e.message}")
                return
            }
            LocalFileSystem.getInstance().refreshAndFindFileByNioFile(root.resolve(JrsFiles.MANIFEST))?.refresh(false, false)
            if (output.exitCode != 0) {
                JrsNotifications.error(project, "jrs ${args.first()} failed:\n${JrsCli.failureMessage(output)}")
                return
            }
            JrsSync.sync(project)
        }
    }.queue()
}

class JrsAddDependencyAction : JrsProjectAction() {
    override fun perform(project: Project) {
        val dialog = AddDependencyDialog(project)
        if (!dialog.showAndGet()) return
        val coordinates = dialog.coordinates
        if (coordinates.isEmpty()) return
        editManifest(project, "jrs add", listOf("add") + dialog.flag.flag + coordinates)
    }

    private enum class Kind(val label: String, val flag: List<String>) {
        DEPENDENCY("[dependencies]", emptyList()),
        DEV("[dev-dependencies]", listOf("--dev")),
        COMPILE_ONLY("[dependencies], compile only", listOf("--compile-only")),
        RUNTIME_ONLY("[dependencies], runtime only", listOf("--runtime-only")),
        ;

        override fun toString() = label
    }

    private class AddDependencyDialog(project: Project) : DialogWrapper(project) {
        private val input = JBTextField().apply {
            emptyText.text = "group:artifact[:version]"
            columns = 40
        }
        private val kind = ComboBox(Kind.entries.toTypedArray())

        val coordinates: List<String> get() = input.text.split(' ', ',').map { it.trim() }.filter { it.isNotEmpty() }
        val flag: Kind get() = kind.selectedItem as Kind

        init {
            title = "Add Dependency (jrs add)"
            init()
        }

        override fun getPreferredFocusedComponent(): JComponent = input

        override fun createCenterPanel(): JComponent = FormBuilder.createFormBuilder()
            .addLabeledComponent("Coordinates:", input)
            .addTooltip("Without a version, jrs adds the newest release")
            .addLabeledComponent("Add to:", kind)
            .panel
    }
}

class JrsRemoveDependencyAction : JrsProjectAction() {
    override fun actionPerformed(e: AnActionEvent) {
        remove(e.project ?: return, e.getData(SELECTED_DEPENDENCY))
    }

    override fun perform(project: Project) = remove(project, null)

    private fun remove(project: Project, selected: String?) {
        val key = Messages.showInputDialog(
            project,
            "The dependency to remove, as jrs.toml names it (group:artifact):",
            "Remove Dependency (jrs remove)",
            null,
            selected.orEmpty(),
            null,
        )?.trim()
        if (key.isNullOrEmpty()) return
        editManifest(project, "jrs remove", listOf("remove", key))
    }
}

/** `jrs fetch --sources`, then a reload, so the libraries get their sources attached. */
class JrsFetchSourcesAction : JrsProjectAction() {
    override fun perform(project: Project) {
        val root = JrsFiles.projectRoot(project) ?: return
        object : Task.Backgroundable(project, "Downloading dependency sources", true) {
            override fun run(indicator: ProgressIndicator) {
                val output = try {
                    JrsCli.run(root, listOf("fetch", "--sources"), indicator)
                } catch (e: ExecutionException) {
                    JrsNotifications.error(project, "Could not run ${JrsCli.executable()}: ${e.message}")
                    return
                }
                if (output.exitCode != 0) {
                    JrsNotifications.error(project, "jrs fetch --sources failed:\n${JrsCli.failureMessage(output)}")
                    return
                }
                JrsSync.sync(project)
            }
        }.queue()
    }
}
