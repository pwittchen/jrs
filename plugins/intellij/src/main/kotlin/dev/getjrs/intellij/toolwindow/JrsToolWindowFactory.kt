package dev.getjrs.intellij.toolwindow

import com.intellij.icons.AllIcons
import com.intellij.openapi.actionSystem.ActionManager
import com.intellij.openapi.actionSystem.DataSink
import com.intellij.openapi.actionSystem.DefaultActionGroup
import com.intellij.openapi.actionSystem.Separator
import com.intellij.openapi.actionSystem.UiDataProvider
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.project.DumbAware
import com.intellij.openapi.project.Project
import com.intellij.openapi.roots.DependencyScope
import com.intellij.openapi.ui.SimpleToolWindowPanel
import com.intellij.openapi.wm.ToolWindow
import com.intellij.openapi.wm.ToolWindowFactory
import com.intellij.ui.ColoredTreeCellRenderer
import com.intellij.ui.DoubleClickListener
import com.intellij.ui.ScrollPaneFactory
import com.intellij.ui.SimpleTextAttributes
import com.intellij.ui.content.ContentFactory
import com.intellij.ui.treeStructure.Tree
import com.intellij.util.ui.tree.TreeUtil
import dev.getjrs.intellij.JrsFiles
import dev.getjrs.intellij.JrsIcons
import dev.getjrs.intellij.actions.SELECTED_DEPENDENCY
import dev.getjrs.intellij.model.JrsMetadata
import dev.getjrs.intellij.run.JrsCommands
import dev.getjrs.intellij.sync.JrsModelApplier
import dev.getjrs.intellij.sync.JrsProjectState
import java.awt.event.MouseEvent
import javax.swing.JTree
import javax.swing.tree.DefaultMutableTreeNode
import javax.swing.tree.DefaultTreeModel

/** The jrs tool window: the project's tasks and dependencies, and the commands as a toolbar. */
class JrsToolWindowFactory : ToolWindowFactory, DumbAware {
    override suspend fun isApplicableAsync(project: Project): Boolean = JrsFiles.projectRoot(project) != null

    override fun createToolWindowContent(project: Project, toolWindow: ToolWindow) {
        val panel = JrsToolWindowPanel(project)
        val content = ContentFactory.getInstance().createContent(panel, "", false)
        content.setDisposer(panel)
        toolWindow.contentManager.addContent(content)
    }
}

private sealed interface Node {
    data class Root(val text: String) : Node
    data class Group(val text: String) : Node
    data class TaskNode(val task: JrsMetadata.Task) : Node
    data class Dependency(val entry: JrsMetadata.Entry) : Node
    data class Hint(val text: String) : Node
}

private class JrsToolWindowPanel(private val project: Project) :
    SimpleToolWindowPanel(true, true), UiDataProvider, com.intellij.openapi.Disposable {

    private val treeModel = DefaultTreeModel(DefaultMutableTreeNode(Node.Hint("")))
    private val tree = Tree(treeModel).apply {
        isRootVisible = true
        cellRenderer = Renderer()
    }

    init {
        val manager = ActionManager.getInstance()
        val group = DefaultActionGroup()
        listOf(
            "Jrs.Sync", null, "Jrs.Build", "Jrs.Test", "Jrs.Run", "Jrs.Package", "Jrs.Clean", null,
            "Jrs.Tree", "Jrs.Fetch", "Jrs.AddDependency", "Jrs.RemoveDependency",
        ).forEach { id -> if (id == null) group.add(Separator.getInstance()) else manager.getAction(id)?.let(group::add) }
        val more = DefaultActionGroup("More", true).apply {
            templatePresentation.icon = AllIcons.Actions.More
            listOf("Jrs.Outdated", "Jrs.Update", "Jrs.Verify").forEach { manager.getAction(it)?.let(::add) }
        }
        group.add(more)
        val toolbar = manager.createActionToolbar("JrsToolWindow", group, true)
        toolbar.targetComponent = this
        setToolbar(toolbar.component)
        setContent(ScrollPaneFactory.createScrollPane(tree))

        object : DoubleClickListener() {
            override fun onDoubleClick(event: MouseEvent): Boolean {
                val node = selected() as? Node.TaskNode ?: return false
                JrsCommands.run(project, "task", node.task.name)
                return true
            }
        }.installOn(tree)

        project.messageBus.connect(this).subscribe(
            JrsProjectState.TOPIC,
            JrsProjectState.Listener { ApplicationManager.getApplication().invokeLater { rebuild() } },
        )
        rebuild()
    }

    private fun selected(): Node? =
        (tree.lastSelectedPathComponent as? DefaultMutableTreeNode)?.userObject as? Node

    override fun uiDataSnapshot(sink: DataSink) {
        val dependency = selected() as? Node.Dependency ?: return
        dependency.entry.key?.let { sink[SELECTED_DEPENDENCY] = it }
    }

    private fun rebuild() {
        val metadata = JrsProjectState.getInstance(project).metadata
        if (metadata == null) {
            treeModel.setRoot(DefaultMutableTreeNode(Node.Hint("Load the jrs project to see its tasks and dependencies")))
            return
        }
        val root = DefaultMutableTreeNode(Node.Root("${metadata.project.name} ${metadata.project.version}"))
        val tasks = DefaultMutableTreeNode(Node.Group("Tasks"))
        metadata.tasks.forEach { tasks.add(DefaultMutableTreeNode(Node.TaskNode(it))) }
        if (metadata.tasks.isEmpty()) tasks.add(DefaultMutableTreeNode(Node.Hint("No [tasks] in jrs.toml")))
        root.add(tasks)

        val dependencies = DefaultMutableTreeNode(Node.Group("Dependencies"))
        val classpaths = metadata.classpaths
        if (classpaths == null) {
            dependencies.add(DefaultMutableTreeNode(Node.Hint("Not resolved yet")))
        } else {
            val byScope = JrsModelApplier.libraries(classpaths).groupBy { it.scope }
            for ((scope, label) in SCOPES) {
                val libraries = byScope[scope].orEmpty()
                if (libraries.isEmpty()) continue
                val group = DefaultMutableTreeNode(Node.Group("$label (${libraries.size})"))
                libraries.forEach { group.add(DefaultMutableTreeNode(Node.Dependency(it.entry))) }
                dependencies.add(group)
            }
        }
        root.add(dependencies)
        treeModel.setRoot(root)
        TreeUtil.expand(tree, 2)
    }

    override fun dispose() = Unit

    private class Renderer : ColoredTreeCellRenderer() {
        override fun customizeCellRenderer(
            tree: JTree,
            value: Any?,
            selected: Boolean,
            expanded: Boolean,
            leaf: Boolean,
            row: Int,
            hasFocus: Boolean,
        ) {
            when (val node = (value as? DefaultMutableTreeNode)?.userObject) {
                is Node.Root -> {
                    icon = JrsIcons.Jrs
                    append(node.text, SimpleTextAttributes.REGULAR_BOLD_ATTRIBUTES)
                }
                is Node.Group -> {
                    icon = AllIcons.Nodes.Folder
                    append(node.text)
                }
                is Node.TaskNode -> {
                    icon = AllIcons.Nodes.ConfigFolder
                    append(node.task.name)
                    node.task.description?.let { append("  $it", SimpleTextAttributes.GRAYED_ATTRIBUTES) }
                    if (node.task.hooks.isNotEmpty()) {
                        append("  [${node.task.hooks.joinToString(", ")}]", SimpleTextAttributes.GRAY_ITALIC_ATTRIBUTES)
                    }
                }
                is Node.Dependency -> {
                    icon = AllIcons.Nodes.PpLib
                    append(node.entry.displayName)
                    if (node.entry.sources != null) append("  sources", SimpleTextAttributes.GRAYED_SMALL_ATTRIBUTES)
                }
                is Node.Hint -> append(node.text, SimpleTextAttributes.GRAYED_ATTRIBUTES)
                else -> Unit
            }
        }
    }

    companion object {
        private val SCOPES = listOf(
            DependencyScope.COMPILE to "Compile",
            DependencyScope.PROVIDED to "Compile only",
            DependencyScope.RUNTIME to "Runtime only",
            DependencyScope.TEST to "Test",
        )
    }
}
