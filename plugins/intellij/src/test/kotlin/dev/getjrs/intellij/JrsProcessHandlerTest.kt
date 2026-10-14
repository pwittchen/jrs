package dev.getjrs.intellij

import com.intellij.execution.configurations.GeneralCommandLine
import com.intellij.execution.process.ProcessEvent
import com.intellij.execution.process.ProcessListener
import com.intellij.execution.process.ProcessOutputType
import com.intellij.openapi.util.Key
import com.intellij.openapi.util.SystemInfo
import com.intellij.testFramework.HeavyPlatformTestCase
import dev.getjrs.intellij.run.JrsProcessHandler

/** What a console receives from a process jrs-like output comes from: stderr, sorted. */
class JrsProcessHandlerTest : HeavyPlatformTestCase() {
    fun `test jrs's transcript on stderr reaches the console as output, its error as an error`() {
        if (SystemInfo.isWindows) return
        val script = listOf(
            "printf '   Compiling app v1.0.0 (2 source files)\\n' >&2",
            "printf 'program output\\n'",
            "printf '    Finished build in 1.2s\\n' >&2",
            "printf 'error: compilation failed (1 source files)\\n' >&2",
        ).joinToString("; ")
        val handler = JrsProcessHandler(GeneralCommandLine("sh", "-c", script))
        val received = mutableListOf<Pair<String, Boolean>>()
        handler.addProcessListener(object : ProcessListener {
            override fun onTextAvailable(event: ProcessEvent, outputType: Key<*>) {
                if (!ProcessOutputType.isStdout(outputType) && !ProcessOutputType.isStderr(outputType)) return
                synchronized(received) { received += event.text.trimEnd('\n') to ProcessOutputType.isStderr(outputType) }
            }
        })
        handler.startNotify()
        assertTrue(handler.waitFor(10_000))
        val byLine = synchronized(received) { received.toMap() }
        assertEquals(false, byLine["   Compiling app v1.0.0 (2 source files)"])
        assertEquals(false, byLine["    Finished build in 1.2s"])
        assertEquals(false, byLine["program output"])
        assertEquals(true, byLine["error: compilation failed (1 source files)"])
    }
}
