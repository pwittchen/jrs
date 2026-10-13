package dev.getjrs.intellij.run

import com.intellij.execution.ProgramRunnerUtil
import com.intellij.execution.RunManager
import com.intellij.execution.executors.DefaultRunExecutor
import com.intellij.openapi.project.Project

/** Runs a jrs command in the Run tool window, through a temporary run configuration. */
object JrsCommands {
    fun run(project: Project, command: String, arguments: String = "", programArguments: String = "") {
        val runManager = RunManager.getInstance(project)
        val settings = runManager.createConfiguration("jrs", JrsConfigurationType.getInstance().factory)
        val configuration = settings.configuration as JrsRunConfiguration
        configuration.command = command
        configuration.arguments = arguments
        configuration.programArguments = programArguments
        settings.name = configuration.commandName()
        settings.isTemporary = true
        ProgramRunnerUtil.executeConfiguration(settings, DefaultRunExecutor.getRunExecutorInstance())
    }
}
