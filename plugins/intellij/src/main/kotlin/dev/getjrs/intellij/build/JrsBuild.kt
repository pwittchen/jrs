package dev.getjrs.intellij.build

import com.intellij.build.BuildViewManager
import com.intellij.build.DefaultBuildDescriptor
import com.intellij.build.FilePosition
import com.intellij.build.events.FinishBuildEvent
import com.intellij.build.events.MessageEvent
import com.intellij.build.events.OutputBuildEvent
import com.intellij.build.events.StartBuildEvent
import com.intellij.build.events.impl.FailureResultImpl
import com.intellij.build.events.impl.SuccessResultImpl
import com.intellij.execution.ExecutionException
import com.intellij.execution.process.OSProcessHandler
import com.intellij.execution.process.ProcessEvent
import com.intellij.execution.process.ProcessListener
import com.intellij.execution.process.ProcessOutputType
import com.intellij.execution.process.ProcessOutputTypes
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.application.ModalityState
import com.intellij.openapi.components.service
import com.intellij.openapi.fileEditor.FileDocumentManager
import com.intellij.openapi.progress.ProgressIndicator
import com.intellij.openapi.progress.Task
import com.intellij.openapi.project.Project
import com.intellij.openapi.util.Key
import com.intellij.openapi.vfs.LocalFileSystem
import com.intellij.openapi.vfs.VfsUtil
import dev.getjrs.intellij.JrsCli
import dev.getjrs.intellij.JrsFiles
import dev.getjrs.intellij.sync.JrsProjectState
import java.nio.file.Path

/**
 * Runs `jrs build` (after `jrs clean` for a rebuild) and reports it in the
 * Build tool window: the plain transcript as output, and each compiler
 * diagnostic as a message linked to its source.
 */
object JrsBuild {
    fun run(project: Project, rebuild: Boolean, done: (Boolean) -> Unit) {
        val root = JrsFiles.projectRoot(project)
        if (root == null) {
            done(false)
            return
        }
        ApplicationManager.getApplication().invokeAndWait({ FileDocumentManager.getInstance().saveAllDocuments() }, ModalityState.any())
        val title = if (rebuild) "jrs clean build" else "jrs build"
        object : Task.Backgroundable(project, title, true) {
            var ok = false

            override fun run(indicator: ProgressIndicator) {
                val buildId = Any()
                val view = project.service<BuildViewManager>()
                val descriptor = DefaultBuildDescriptor(buildId, title, root.toString(), System.currentTimeMillis())
                view.onEvent(buildId, StartBuildEvent.builder("$title…", descriptor).build())
                ok = (!rebuild || command(project, root, buildId, listOf("clean"), indicator)) &&
                    command(project, root, buildId, listOf("build"), indicator)
                val result = if (ok) SuccessResultImpl() else FailureResultImpl()
                view.onEvent(buildId, FinishBuildEvent.builder(buildId, if (ok) "finished" else "failed", result).build())
                refreshOutputs(project, root)
            }

            override fun onFinished() = done(ok)
        }.queue()
    }

    private fun command(project: Project, root: Path, buildId: Any, args: List<String>, indicator: ProgressIndicator): Boolean {
        val view = project.service<BuildViewManager>()
        val handler = try {
            OSProcessHandler(JrsCli.commandLine(root, args))
        } catch (e: ExecutionException) {
            view.onEvent(buildId, output(buildId, "Could not run ${JrsCli.executable()}: ${e.message}\n", false))
            return false
        }
        val pending = StringBuilder()
        fun line(text: String, stdout: Boolean) {
            view.onEvent(buildId, output(buildId, text + "\n", stdout))
            val diagnostic = CompilerDiagnostics.parse(text) ?: return
            val path = runCatching { Path.of(diagnostic.path) }.getOrNull() ?: return
            val position = FilePosition(path, diagnostic.line - 1, (diagnostic.column ?: 1) - 1)
            val kind = if (diagnostic.error) MessageEvent.Kind.ERROR else MessageEvent.Kind.WARNING
            val message = MessageEvent.builder(diagnostic.message, kind)
                .withParentId(buildId)
                .withGroup("compiler")
                .withDescription(text)
                .withFilePosition(position)
                .build()
            view.onEvent(buildId, message)
        }
        handler.addProcessListener(object : ProcessListener {
            override fun onTextAvailable(event: ProcessEvent, outputType: Key<*>) {
                if (outputType == ProcessOutputTypes.SYSTEM) return
                synchronized(pending) {
                    pending.append(event.text)
                    while (true) {
                        val newline = pending.indexOf("\n")
                        if (newline < 0) break
                        line(pending.substring(0, newline).trimEnd('\r'), outputType == ProcessOutputTypes.STDOUT)
                        pending.delete(0, newline + 1)
                    }
                }
            }

            override fun processTerminated(event: ProcessEvent) {
                synchronized(pending) {
                    if (pending.isNotEmpty()) line(pending.toString(), true)
                    pending.setLength(0)
                }
            }
        })
        handler.startNotify()
        while (!handler.waitFor(200)) {
            if (indicator.isCanceled) {
                handler.destroyProcess()
                handler.waitFor()
                return false
            }
        }
        return handler.exitCode == 0
    }

    private fun output(buildId: Any, text: String, stdout: Boolean): OutputBuildEvent =
        OutputBuildEvent.builder(text)
            .withParentId(buildId)
            .withOutputType(if (stdout) ProcessOutputType.STDOUT else ProcessOutputType.STDERR)
            .build()

    private fun refreshOutputs(project: Project, root: Path) {
        val metadata = JrsProjectState.getInstance(project).metadata
        val target = metadata?.targetDir?.let(Path::of) ?: root.resolve("target")
        LocalFileSystem.getInstance().refreshAndFindFileByNioFile(target)?.let {
            VfsUtil.markDirtyAndRefresh(true, true, true, it)
        }
    }
}
