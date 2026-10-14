package dev.getjrs.intellij.run

import com.intellij.execution.configurations.GeneralCommandLine
import com.intellij.execution.process.ProcessOutputTypes
import java.nio.file.Path

/**
 * Runs `jrs test` and, as it ends, feeds the JUnit XML it wrote into the test
 * runner tree — from every directory in [reports], a workspace's members each
 * writing their own. The XML is read before the process is reported as terminated,
 * so the tree is complete when the console closes the run.
 */
class JrsTestProcessHandler(commandLine: GeneralCommandLine, private val reports: List<Path>) :
    JrsProcessHandler(commandLine) {

    private val startedAt = System.currentTimeMillis()

    override fun notifyProcessTerminated(exitCode: Int) {
        try {
            val cases = reports.flatMap { JunitXmlReport.read(it, startedAt) }
            for (message in JunitXmlReport.serviceMessages(cases)) {
                notifyTextAvailable(message + "\n", ProcessOutputTypes.STDOUT)
            }
        } finally {
            super.notifyProcessTerminated(exitCode)
        }
    }
}
