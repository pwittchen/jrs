package dev.getjrs.intellij.run

import com.intellij.execution.configurations.GeneralCommandLine
import com.intellij.execution.process.KillableColoredProcessHandler
import com.intellij.execution.process.ProcessOutputTypes
import java.nio.file.Path

/**
 * Runs `jrs test` and, as it ends, feeds the JUnit XML it wrote into the test
 * runner tree. The XML is read before the process is reported as terminated,
 * so the tree is complete when the console closes the run.
 */
class JrsTestProcessHandler(commandLine: GeneralCommandLine, private val reports: Path) :
    KillableColoredProcessHandler(commandLine) {

    private val startedAt = System.currentTimeMillis()

    override fun notifyProcessTerminated(exitCode: Int) {
        try {
            for (message in JunitXmlReport.serviceMessages(JunitXmlReport.read(reports, startedAt))) {
                notifyTextAvailable(message + "\n", ProcessOutputTypes.STDOUT)
            }
        } finally {
            super.notifyProcessTerminated(exitCode)
        }
    }
}
