package dev.getjrs.intellij.run

import com.intellij.execution.Executor
import com.intellij.execution.configurations.ConfigurationFactory
import com.intellij.execution.configurations.RunConfiguration
import com.intellij.execution.configurations.RunConfigurationBase
import com.intellij.execution.configurations.RunConfigurationOptions
import com.intellij.execution.configurations.RunProfileState
import com.intellij.execution.configurations.RuntimeConfigurationError
import com.intellij.execution.runners.ExecutionEnvironment
import com.intellij.openapi.options.SettingsEditor
import com.intellij.openapi.project.Project
import com.intellij.util.execution.ParametersListUtil

class JrsRunConfigurationOptions : RunConfigurationOptions() {
    var command by string("build")
    var arguments by string("")
    var programArguments by string("")
}

/**
 * `jrs <command> <arguments> [-- <program arguments>]` in the project root.
 * `test` shows its results in the test runner tree; `run` and `test` attach
 * the debugger through `--debug`.
 */
class JrsRunConfiguration(project: Project, factory: ConfigurationFactory, name: String) :
    RunConfigurationBase<JrsRunConfigurationOptions>(project, factory, name) {

    public override fun getOptions(): JrsRunConfigurationOptions = super.getOptions() as JrsRunConfigurationOptions

    var command: String
        get() = options.command?.takeIf { it.isNotBlank() } ?: "build"
        set(value) {
            options.command = value
        }

    var arguments: String
        get() = options.arguments.orEmpty()
        set(value) {
            options.arguments = value
        }

    var programArguments: String
        get() = options.programArguments.orEmpty()
        set(value) {
            options.programArguments = value
        }

    val isTest: Boolean get() = command == "test"

    /** `jrs run` and `jrs test` take `--debug`; nothing else starts a JVM of the project's. */
    val supportsDebug: Boolean get() = command == "run" || command == "test"

    /** Everything after the global flags. */
    fun commandArguments(debugAddress: String? = null): List<String> = buildList {
        add(command)
        addAll(ParametersListUtil.parse(arguments))
        if (debugAddress != null) add("--debug=$debugAddress")
        val program = ParametersListUtil.parse(programArguments)
        if (program.isNotEmpty()) {
            add("--")
            addAll(program)
        }
    }

    override fun checkConfiguration() {
        if (command.isBlank()) throw RuntimeConfigurationError("No jrs command")
        if (command == "task" && ParametersListUtil.parse(arguments).isEmpty()) {
            throw RuntimeConfigurationError("jrs task needs the task's name in Arguments")
        }
    }

    override fun getConfigurationEditor(): SettingsEditor<out RunConfiguration> = JrsSettingsEditor()

    override fun getState(executor: Executor, environment: ExecutionEnvironment): RunProfileState =
        JrsRunState(this, environment)

    /** `jrs <command> <arguments>`, the name a configuration made from an action gets. */
    fun commandName(): String = (listOf("jrs", command) + ParametersListUtil.parse(arguments)).joinToString(" ")

    companion object {
        /** The workspace members `-p` / `--module` name in [arguments], in order. */
        fun memberArguments(arguments: String): List<String> {
            val args = ParametersListUtil.parse(arguments)
            return buildList {
                var i = 0
                while (i < args.size) {
                    val arg = args[i]
                    when {
                        (arg == "-p" || arg == "--module") && i + 1 < args.size -> {
                            add(args[i + 1])
                            i++
                        }
                        arg.startsWith("--module=") -> add(arg.removePrefix("--module="))
                        arg.startsWith("-p") && arg.length > 2 -> add(arg.removePrefix("-p"))
                    }
                    i++
                }
            }
        }
    }
}
