package dev.getjrs.intellij.run

import com.intellij.execution.ProgramRunnerUtil
import com.intellij.execution.RunManager
import com.intellij.execution.configurations.RunProfile
import com.intellij.execution.configurations.RunProfileState
import com.intellij.execution.configurations.RunnerSettings
import com.intellij.execution.executors.DefaultDebugExecutor
import com.intellij.execution.process.ProcessEvent
import com.intellij.execution.process.ProcessListener
import com.intellij.execution.remote.RemoteConfiguration
import com.intellij.execution.remote.RemoteConfigurationType
import com.intellij.execution.runners.AsyncProgramRunner
import com.intellij.execution.runners.ExecutionEnvironment
import com.intellij.execution.runners.RunContentBuilder
import com.intellij.execution.ui.RunContentDescriptor
import com.intellij.openapi.application.ApplicationManager
import com.intellij.openapi.fileEditor.FileDocumentManager
import com.intellij.openapi.project.Project
import com.intellij.openapi.util.Key
import org.jetbrains.concurrency.Promise
import org.jetbrains.concurrency.resolvedPromise
import java.util.concurrent.atomic.AtomicBoolean

/**
 * Debug for `jrs run` and `jrs test`: jrs builds, then starts the JVM with
 * `--debug`, which waits on a JDWP port. Once the JVM says it is listening,
 * a Remote JVM Debug session attaches to it.
 */
class JrsDebugRunner : AsyncProgramRunner<RunnerSettings>() {
    override fun getRunnerId(): String = "JrsDebugRunner"

    override fun canRun(executorId: String, profile: RunProfile): Boolean =
        executorId == DefaultDebugExecutor.EXECUTOR_ID && profile is JrsRunConfiguration && profile.supportsDebug

    override fun execute(environment: ExecutionEnvironment, state: RunProfileState): Promise<RunContentDescriptor?> {
        FileDocumentManager.getInstance().saveAllDocuments()
        val jrsState = state as JrsRunState
        val result = jrsState.execute(environment.executor, this)
        val port = jrsState.debugPort
        if (port != null) {
            val attached = AtomicBoolean()
            result.processHandler.addProcessListener(object : ProcessListener {
                override fun onTextAvailable(event: ProcessEvent, outputType: Key<*>) {
                    if (event.text.contains("Listening for transport dt_socket") && attached.compareAndSet(false, true)) {
                        ApplicationManager.getApplication().invokeLater {
                            attach(environment.project, port, environment.runProfile.name)
                        }
                    }
                }
            })
        }
        val descriptor = RunContentBuilder(result, environment).showRunContent(environment.contentToReuse)
        return resolvedPromise(descriptor)
    }

    private fun attach(project: Project, port: Int, name: String) {
        if (project.isDisposed) return
        val settings = RunManager.getInstance(project)
            .createConfiguration("Attach to $name", RemoteConfigurationType.getInstance().configurationFactories.single())
        settings.isTemporary = true
        (settings.configuration as RemoteConfiguration).apply {
            HOST = "127.0.0.1"
            PORT = port.toString()
            SERVER_MODE = false
            USE_SOCKET_TRANSPORT = true
        }
        ProgramRunnerUtil.executeConfiguration(settings, DefaultDebugExecutor.getDebugExecutorInstance())
    }
}
