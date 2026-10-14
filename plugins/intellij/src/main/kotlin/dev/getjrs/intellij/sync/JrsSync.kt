package dev.getjrs.intellij.sync

import com.intellij.execution.ExecutionException
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.application.ModalityState
import com.intellij.openapi.fileEditor.FileDocumentManager
import com.intellij.openapi.progress.ProgressIndicator
import com.intellij.openapi.progress.Task
import com.intellij.openapi.project.Project
import com.intellij.openapi.vfs.LocalFileSystem
import com.intellij.ui.EditorNotifications
import dev.getjrs.intellij.JrsCli
import dev.getjrs.intellij.JrsFiles
import dev.getjrs.intellij.JrsNotifications
import dev.getjrs.intellij.model.JrsMetadata
import java.nio.file.Path
import java.util.concurrent.ConcurrentHashMap

/**
 * Loads the project from `jrs metadata`. On the first import a `--no-deps`
 * pass sets the source roots up before resolution has finished; the full pass
 * then adds the JDK's libraries. Nothing here reads `jrs.toml` itself.
 */
object JrsSync {
    private val running = ConcurrentHashMap.newKeySet<Project>()

    fun isRunning(project: Project): Boolean = project in running

    /** Starts a sync in the background; a second request while one runs is dropped. */
    fun sync(project: Project, quiet: Boolean = false, then: (() -> Unit)? = null) {
        val root = JrsFiles.projectRoot(project)
        if (root == null) {
            if (!quiet) JrsNotifications.error(project, "There is no ${JrsFiles.MANIFEST} in ${project.basePath}.")
            return
        }
        if (!running.add(project)) return
        ApplicationManager.getApplication().invokeAndWait({ FileDocumentManager.getInstance().saveAllDocuments() }, ModalityState.any())
        object : Task.Backgroundable(project, "Loading jrs project", true) {
            override fun run(indicator: ProgressIndicator) {
                val state = JrsProjectState.getInstance(project)
                if (!state.isLinked) {
                    indicator.text = "jrs metadata --no-deps"
                    load(project, root, listOf("metadata", "--no-deps"), indicator, quiet) ?: return
                }
                indicator.text = "jrs metadata"
                load(project, root, listOf("metadata"), indicator, quiet) ?: return
                LocalFileSystem.getInstance().refreshAndFindFileByNioFile(root.resolve(JrsFiles.LOCKFILE))
            }

            override fun onFinished() {
                running.remove(project)
                if (!project.isDisposed) EditorNotifications.getInstance(project).updateAllNotifications()
            }

            override fun onSuccess() {
                then?.invoke()
            }
        }.queue()
    }

    private fun load(
        project: Project,
        root: Path,
        args: List<String>,
        indicator: ProgressIndicator,
        quiet: Boolean,
    ): JrsMetadata? {
        var opened: String? = null
        val members = try {
            val output = JrsCli.run(root, args, indicator)
            if (output.isCancelled) return null
            if (output.exitCode != 0) {
                fail(project, "jrs ${args.joinToString(" ")} failed:\n${JrsCli.failureMessage(output)}", quiet)
                return null
            }
            val parsed = JrsMetadata.parseAll(output.stdout)
            // Opened at one member's directory, jrs describes that member
            // alone; the members it depends on are modules too, so the whole
            // workspace is imported, the opened member first in view.
            val single = parsed.singleOrNull()
            if (single?.workspace != null) {
                val whole = JrsCli.run(root, listOf("--manifest-path", single.workspace) + args, indicator)
                if (whole.isCancelled) return null
                if (whole.exitCode != 0) {
                    fail(project, "jrs ${args.joinToString(" ")} failed:\n${JrsCli.failureMessage(whole)}", quiet)
                    return null
                }
                opened = single.project.name
                JrsMetadata.parseAll(whole.stdout)
            } else {
                parsed
            }
        } catch (e: ExecutionException) {
            fail(project, "Could not run ${JrsCli.executable()}: ${e.message}\nSet the jrs executable in Settings | Build, Execution, Deployment | Build Tools | jrs.", quiet)
            return null
        } catch (e: IllegalArgumentException) {
            fail(project, e.message ?: "jrs metadata printed something this plugin cannot read.", quiet)
            return null
        }
        members.forEach(JrsModelApplier::refreshFiles)
        // In a workspace, the tool window shows the application: the member
        // with a main class, else the first.
        val primary = opened?.let { name -> members.indexOfFirst { it.project.name == name } }?.takeIf { it >= 0 }
            ?: members.indexOfFirst { it.project.mainClass != null }.takeIf { it >= 0 }
            ?: 0
        val metadata = members[primary]
        var failure: Throwable? = null
        ApplicationManager.getApplication().invokeAndWait({
            if (project.isDisposed) return@invokeAndWait
            try {
                val modules = JrsModelApplier.applyAll(project, members)
                val workspace = members.size > 1 || members[0].modules.isNotEmpty()
                val names = if (workspace) modules.map { it.name } else emptyList()
                JrsProjectState.getInstance(project).linked(
                    modules[primary].name,
                    metadata,
                    root,
                    names,
                    if (workspace) members else emptyList(),
                    members[primary].workspace,
                )
            } catch (e: RuntimeException) {
                failure = e
            }
        }, ModalityState.nonModal())
        failure?.let {
            fail(project, "Could not set the project up from jrs metadata: ${it.message}", quiet = false)
            return null
        }
        return metadata
    }

    private fun fail(project: Project, message: String, quiet: Boolean) {
        // A quiet sync (on startup) still reports a failure, only as a warning.
        if (quiet) {
            JrsNotifications.notify(project, message, com.intellij.notification.NotificationType.WARNING)
        } else {
            JrsNotifications.error(project, message)
        }
    }
}
