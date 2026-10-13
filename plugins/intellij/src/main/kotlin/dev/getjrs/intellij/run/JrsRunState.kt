package dev.getjrs.intellij.run

import com.intellij.execution.DefaultExecutionResult
import com.intellij.execution.ExecutionException
import com.intellij.execution.ExecutionResult
import com.intellij.execution.Executor
import com.intellij.execution.configurations.CommandLineState
import com.intellij.execution.executors.DefaultDebugExecutor
import com.intellij.execution.filters.RegexpFilter
import com.intellij.execution.process.KillableColoredProcessHandler
import com.intellij.execution.process.ProcessHandler
import com.intellij.execution.process.ProcessTerminatedListener
import com.intellij.execution.runners.ExecutionEnvironment
import com.intellij.execution.runners.ProgramRunner
import com.intellij.execution.testframework.sm.SMTestRunnerConnectionUtil
import com.intellij.util.net.NetUtils
import dev.getjrs.intellij.JrsCli
import dev.getjrs.intellij.JrsFiles
import dev.getjrs.intellij.sync.JrsProjectState
import java.nio.file.Path

class JrsRunState(private val configuration: JrsRunConfiguration, environment: ExecutionEnvironment) :
    CommandLineState(environment) {

    /** The JDWP port the program waits on when debugged; null when not debugging. */
    val debugPort: Int? =
        if (configuration.supportsDebug && environment.executor.id == DefaultDebugExecutor.EXECUTOR_ID) {
            NetUtils.findAvailableSocketPort()
        } else {
            null
        }

    init {
        // javac's `path:line: error:` lines become links to the source.
        addConsoleFilters(RegexpFilter(environment.project, "${RegexpFilter.FILE_PATH_MACROS}:${RegexpFilter.LINE_MACROS}"))
    }

    private val root: Path
        get() = JrsFiles.projectRoot(environment.project)
            ?: throw ExecutionException("There is no ${JrsFiles.MANIFEST} in ${environment.project.basePath}")

    override fun startProcess(): ProcessHandler {
        val commandLine = JrsCli.commandLine(root, configuration.commandArguments(debugPort?.let { "127.0.0.1:$it" }))
        val handler = if (configuration.isTest) {
            val metadata = JrsProjectState.getInstance(environment.project).metadata
            val reports = Path.of(metadata?.targetDir ?: root.resolve("target").toString(), "test-reports")
            JrsTestProcessHandler(commandLine, reports)
        } else {
            KillableColoredProcessHandler(commandLine)
        }
        ProcessTerminatedListener.attach(handler)
        return handler
    }

    override fun execute(executor: Executor, runner: ProgramRunner<*>): ExecutionResult {
        if (!configuration.isTest) return super.execute(executor, runner)
        val handler = startProcess()
        val properties = JrsTestConsoleProperties(configuration, executor)
        val console = SMTestRunnerConnectionUtil.createAndAttachConsole("jrs", handler, properties)
        return DefaultExecutionResult(console, handler)
    }
}
