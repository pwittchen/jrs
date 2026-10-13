package dev.getjrs.intellij.build

import com.intellij.openapi.project.Project
import com.intellij.task.ModuleBuildTask
import com.intellij.task.ProjectTask
import com.intellij.task.ProjectTaskContext
import com.intellij.task.ProjectTaskRunner
import com.intellij.task.TaskRunnerResults
import dev.getjrs.intellij.settings.JrsSettings
import dev.getjrs.intellij.sync.JrsProjectState
import org.jetbrains.concurrency.AsyncPromise
import org.jetbrains.concurrency.Promise

/**
 * Delegates Build Project (and the build before a run) to `jrs build`, as
 * IntelliJ delegates to Gradle, so the IDE and the terminal write the same
 * `target/classes`.
 */
class JrsProjectTaskRunner : ProjectTaskRunner() {
    override fun canRun(project: Project, projectTask: ProjectTask): Boolean =
        JrsSettings.getInstance().state.buildWithJrs &&
            projectTask is ModuleBuildTask &&
            JrsProjectState.getInstance(project).isJrsModule(projectTask.module)

    override fun run(project: Project, context: ProjectTaskContext, vararg tasks: ProjectTask): Promise<Result> {
        val promise = AsyncPromise<Result>()
        val rebuild = tasks.any { it is ModuleBuildTask && !it.isIncrementalBuild }
        JrsBuild.run(project, rebuild) { ok ->
            promise.setResult(if (ok) TaskRunnerResults.SUCCESS else TaskRunnerResults.FAILURE)
        }
        return promise
    }
}
