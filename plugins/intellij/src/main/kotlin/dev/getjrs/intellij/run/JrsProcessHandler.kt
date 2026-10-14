package dev.getjrs.intellij.run

import com.intellij.execution.configurations.GeneralCommandLine
import com.intellij.execution.process.KillableColoredProcessHandler
import com.intellij.execution.process.ProcessOutputType
import com.intellij.execution.process.ProcessOutputTypes
import com.intellij.openapi.util.Key

/**
 * Runs jrs in a console, showing as an error only what is one. jrs writes its
 * transcript to stderr, so each stderr line goes through [JrsOutput]: its
 * phase lines, summary and warnings are shown as ordinary output, its errors
 * and compiler errors as errors, and whatever else comes on stderr as it was.
 */
open class JrsProcessHandler(commandLine: GeneralCommandLine) : KillableColoredProcessHandler(commandLine) {
    private val output = JrsOutput()
    private val pending = StringBuilder()

    override fun coloredTextAvailable(text: String, attributes: Key<*>) {
        if (!ProcessOutputType.isStderr(attributes)) {
            super.coloredTextAvailable(text, attributes)
            return
        }
        val lines = synchronized(pending) {
            pending.append(text)
            buildList {
                while (true) {
                    val newline = pending.indexOf("\n")
                    if (newline < 0) break
                    add(pending.substring(0, newline + 1))
                    pending.delete(0, newline + 1)
                }
            }
        }
        lines.forEach(::emit)
    }

    private fun emit(line: String) {
        val type = if (output.isError(line.trimEnd('\n', '\r'))) ProcessOutputTypes.STDERR else ProcessOutputTypes.STDOUT
        super.coloredTextAvailable(line, type)
    }

    override fun notifyProcessTerminated(exitCode: Int) {
        val rest = synchronized(pending) { pending.toString().also { pending.setLength(0) } }
        if (rest.isNotEmpty()) emit(rest)
        super.notifyProcessTerminated(exitCode)
    }
}
