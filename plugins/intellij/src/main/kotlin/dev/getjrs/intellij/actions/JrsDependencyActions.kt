package dev.getjrs.intellij.actions

import com.intellij.execution.ExecutionException
import com.intellij.openapi.actionSystem.AnActionEvent
import com.intellij.openapi.actionSystem.CommonDataKeys
import com.intellij.openapi.actionSystem.DataKey
import com.intellij.openapi.module.ModuleUtilCore
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
import dev.getjrs.intellij.sync.JrsProjectState
import dev.getjrs.intellij.sync.JrsSync
import java.nio.file.Path
import javax.swing.JComponent

/** The dependency selected in the jrs tool window, as `group:artifact`. */
val SELECTED_DEPENDENCY: DataKey<String> = DataKey.create("jrs.selectedDependency")

/** In a workspace, the member the jrs tool window's selection is under. */
val SELECTED_MEMBER: DataKey<String> = DataKey.create("jrs.selectedMember")

/**
 * The member an action in a workspace is about: the one selected in the tool
 * window, else the one holding the file in the editor, else the member the
 * tool window shows first. Null outside a workspace.
 */
internal fun contextMember(e: AnActionEvent?, project: Project): String? {
    val state = JrsProjectState.getInstance(project)
    if (!state.isWorkspace) return null
    e?.getData(SELECTED_MEMBER)?.let { return it }
    val file = e?.getData(CommonDataKeys.VIRTUAL_FILE)
    file?.let { ModuleUtilCore.findModuleForFile(it, project) }?.let(state::memberOf)?.let { return it }
    return state.metadata?.project?.name ?: state.members.firstOrNull()?.project?.name
}

/** `-p <member>` for a workspace member, nothing otherwise. */
internal fun memberFlag(member: String?): List<String> = member?.let { listOf("-p", it) }.orEmpty()

/** A combo box of the workspace's members, or null outside a workspace. */
private fun memberChooser(project: Project, selected: String?): ComboBox<String>? {
    val names = JrsProjectState.getInstance(project).members.map { it.project.name }
    if (names.isEmpty()) return null
    return ComboBox(names.toTypedArray()).apply { selected?.takeIf { it in names }?.let { selectedItem = it } }
}

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
            val manifests = listOf(root) + JrsProjectState.getInstance(project).state.memberRoots.map { Path.of(it) }
            for (dir in manifests) {
                LocalFileSystem.getInstance().refreshAndFindFileByNioFile(dir.resolve(JrsFiles.MANIFEST))?.refresh(false, false)
            }
            if (output.exitCode != 0) {
                JrsNotifications.error(project, "jrs ${args.first()} failed:\n${JrsCli.failureMessage(output)}")
                return
            }
            JrsSync.sync(project)
        }
    }.queue()
}

class JrsAddDependencyAction : JrsProjectAction() {
    override fun actionPerformed(e: AnActionEvent) {
        val project = e.project ?: return
        add(project, contextMember(e, project))
    }

    override fun perform(project: Project) = add(project, contextMember(null, project))

    private fun add(project: Project, member: String?) {
        val dialog = AddDependencyDialog(project, member)
        if (!dialog.showAndGet()) return
        val coordinates = dialog.coordinates
        if (coordinates.isEmpty()) return
        editManifest(project, "jrs add", listOf("add") + memberFlag(dialog.member) + dialog.flag.flag + coordinates)
    }

    private enum class Kind(val label: String, val flag: List<String>) {
        DEPENDENCY("[dependencies]", emptyList()),
        DEV("[dev-dependencies]", listOf("--dev")),
        COMPILE_ONLY("[dependencies], compile only", listOf("--compile-only")),
        RUNTIME_ONLY("[dependencies], runtime only", listOf("--runtime-only")),
        ;

        override fun toString() = label
    }

    private class AddDependencyDialog(project: Project, member: String?) : DialogWrapper(project) {
        private val members = memberChooser(project, member)
        private val input = JBTextField().apply {
            emptyText.text = "group:artifact[:version]"
            columns = 40
        }
        private val kind = ComboBox(Kind.entries.toTypedArray())

        val coordinates: List<String> get() = input.text.split(' ', ',').map { it.trim() }.filter { it.isNotEmpty() }
        val flag: Kind get() = kind.selectedItem as Kind
        val member: String? get() = members?.selectedItem as String?

        init {
            title = "Add Dependency (jrs add)"
            init()
        }

        override fun getPreferredFocusedComponent(): JComponent = input

        override fun createCenterPanel(): JComponent = FormBuilder.createFormBuilder()
            .apply { members?.let { addLabeledComponent("Member:", it) } }
            .addLabeledComponent("Coordinates:", input)
            .addTooltip("Without a version, jrs adds the newest release")
            .addLabeledComponent("Add to:", kind)
            .panel
    }
}

class JrsRemoveDependencyAction : JrsProjectAction() {
    override fun actionPerformed(e: AnActionEvent) {
        val project = e.project ?: return
        remove(project, e.getData(SELECTED_DEPENDENCY), contextMember(e, project))
    }

    override fun perform(project: Project) = remove(project, null, contextMember(null, project))

    private fun remove(project: Project, selected: String?, member: String?) {
        if (!JrsProjectState.getInstance(project).isWorkspace) {
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
            return
        }
        val dialog = RemoveDependencyDialog(project, selected, member)
        if (!dialog.showAndGet()) return
        val key = dialog.key
        if (key.isEmpty()) return
        editManifest(project, "jrs remove", listOf("remove") + memberFlag(dialog.member) + key)
    }

    /** In a workspace: which member's `jrs.toml` to remove the dependency from, and the key. */
    private class RemoveDependencyDialog(project: Project, selected: String?, member: String?) : DialogWrapper(project) {
        private val input = JBTextField(selected.orEmpty()).apply { columns = 40 }
        private val members = memberChooser(project, member)

        val key: String get() = input.text.trim()
        val member: String? get() = members?.selectedItem as String?

        init {
            title = "Remove Dependency (jrs remove)"
            init()
        }

        override fun getPreferredFocusedComponent(): JComponent = input

        override fun createCenterPanel(): JComponent = FormBuilder.createFormBuilder()
            .apply { members?.let { addLabeledComponent("Member:", it) } }
            .addLabeledComponent("Dependency:", input)
            .addTooltip("As jrs.toml names it: group:artifact, or a local jar's name")
            .panel
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
