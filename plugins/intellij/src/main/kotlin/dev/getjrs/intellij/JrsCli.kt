package dev.getjrs.intellij

import com.intellij.execution.ExecutionException
import com.intellij.execution.configurations.GeneralCommandLine
import com.intellij.execution.configurations.PathEnvironmentVariableUtil
import com.intellij.execution.process.CapturingProcessHandler
import com.intellij.execution.process.ProcessOutput
import com.intellij.openapi.progress.ProgressIndicator
import com.intellij.openapi.util.SystemInfo
import dev.getjrs.intellij.settings.JrsSettings
import java.nio.charset.StandardCharsets
import java.nio.file.Files
import java.nio.file.Path

/**
 * Runs the `jrs` binary. Every invocation asks for the plain transcript
 * (`--progress never --color never`), which is what a console and a parser need.
 */
object JrsCli {
    private val binaryName = if (SystemInfo.isWindows) "jrs.exe" else "jrs"

    /** `jrs` on `PATH`, or in one of the directories the installers write to. */
    fun findOnPath(): String? {
        PathEnvironmentVariableUtil.findInPath(binaryName)?.let { return it.path }
        val home = System.getProperty("user.home")
        return listOf(
            Path.of(home, ".local", "bin", binaryName),
            Path.of(home, ".cargo", "bin", binaryName),
            Path.of("/opt/homebrew/bin", binaryName),
            Path.of("/usr/local/bin", binaryName),
            Path.of(home, "scoop", "shims", binaryName),
        ).firstOrNull { Files.isExecutable(it) }?.toString()
    }

    fun executable(): String =
        JrsSettings.getInstance().state.executable?.takeIf { it.isNotBlank() } ?: findOnPath() ?: binaryName

    /** `jrs --progress never --color never <args>` in [root]. */
    fun commandLine(root: Path, args: List<String>): GeneralCommandLine =
        GeneralCommandLine(executable())
            .withWorkingDirectory(root)
            .withParameters("--progress", "never", "--color", "never")
            .withParameters(args)
            .withCharset(StandardCharsets.UTF_8)
            .withParentEnvironmentType(GeneralCommandLine.ParentEnvironmentType.CONSOLE)

    /** Runs jrs to completion and captures its output; throws when it cannot be started. */
    @Throws(ExecutionException::class)
    fun run(root: Path, args: List<String>, indicator: ProgressIndicator? = null): ProcessOutput {
        val handler = CapturingProcessHandler(commandLine(root, args))
        return if (indicator != null) handler.runProcessWithProgressIndicator(indicator) else handler.runProcess()
    }

    /** The message to show when jrs fails: its stderr, else its stdout, else the exit code. */
    fun failureMessage(output: ProcessOutput): String =
        output.stderr.trim().ifEmpty { output.stdout.trim() }.ifEmpty { "jrs exited with code ${output.exitCode}" }
}
