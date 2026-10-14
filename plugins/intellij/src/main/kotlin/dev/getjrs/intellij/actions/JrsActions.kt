package dev.getjrs.intellij.actions

import com.intellij.openapi.actionSystem.ActionUpdateThread
import com.intellij.openapi.actionSystem.AnActionEvent
import com.intellij.openapi.project.DumbAwareAction
import com.intellij.openapi.project.Project
import com.intellij.util.execution.ParametersListUtil
import dev.getjrs.intellij.JrsFiles
import dev.getjrs.intellij.run.JrsCommands
import dev.getjrs.intellij.sync.JrsProjectState
import dev.getjrs.intellij.sync.JrsSync

/** An action available in a project with a `jrs.toml`. */
abstract class JrsProjectAction : DumbAwareAction() {
    override fun getActionUpdateThread(): ActionUpdateThread = ActionUpdateThread.BGT

    override fun update(e: AnActionEvent) {
        val project = e.project
        e.presentation.isEnabledAndVisible = project != null && JrsFiles.projectRoot(project) != null
    }

    override fun actionPerformed(e: AnActionEvent) {
        perform(e.project ?: return)
    }

    abstract fun perform(project: Project)
}

/** Runs one jrs command in the Run tool window. */
abstract class JrsCommandAction(private val command: String, private val arguments: String = "") : JrsProjectAction() {
    override fun perform(project: Project) = JrsCommands.run(project, command, arguments)
}

class JrsSyncAction : JrsProjectAction() {
    override fun perform(project: Project) = JrsSync.sync(project)
}

class JrsBuildAction : JrsCommandAction("build")

/**
 * `jrs test`. In a workspace, every test of every member: run at the
 * workspace's root, so a project opened at one member's directory is not
 * narrowed to it, and with `--all`, so neither the selection of what a change
 * reaches nor a cached passing run leaves a member's tests out of the tree.
 */
class JrsTestAction : JrsProjectAction() {
    override fun perform(project: Project) {
        val state = JrsProjectState.getInstance(project)
        val workspace = state.state.workspaceRoot?.takeIf { state.isWorkspace }
        if (workspace == null) {
            JrsCommands.run(project, "test")
        } else {
            JrsCommands.run(project, "test", arguments(workspace), name = "jrs test (all modules)")
        }
    }

    companion object {
        /** The arguments of a run of every test in the workspace at [workspaceRoot]. */
        fun arguments(workspaceRoot: String): String =
            ParametersListUtil.join("--manifest-path", workspaceRoot, "--all")
    }
}

class JrsRunAction : JrsCommandAction("run")

class JrsPackageAction : JrsCommandAction("package")

class JrsCleanAction : JrsCommandAction("clean")

class JrsTreeAction : JrsCommandAction("tree")

class JrsOutdatedAction : JrsCommandAction("outdated")

class JrsVerifyAction : JrsCommandAction("verify")

/** `jrs update` rewrites `jrs.lock`; the change listener then offers to reload. */
class JrsUpdateAction : JrsCommandAction("update")
