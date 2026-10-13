package dev.getjrs.intellij

import dev.getjrs.intellij.run.JunitXmlReport
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class JunitXmlReportTest {
    private val xml = """
        <?xml version="1.0" encoding="UTF-8"?>
        <testsuite name="JUnit Jupiter" tests="3">
          <testcase name="adds()" classname="com.example.CalcTest" time="0.012"/>
          <testcase name="fails()" classname="com.example.CalcTest" time="0.003">
            <failure message="expected: &lt;2&gt; but was: &lt;3&gt;" type="org.opentest4j.AssertionFailedError">stack
        trace</failure>
          </testcase>
          <testcase name="later()" classname="com.example.OtherTest" time="0"><skipped/></testcase>
        </testsuite>
    """.trimIndent()

    @Test
    fun `reads cases and turns them into service messages`() {
        val cases = JunitXmlReport.parse(xml.toByteArray())
        assertEquals(listOf("adds", "fails", "later"), cases.map { it.methodName })
        assertEquals(12L, cases[0].durationMillis)
        assertTrue(cases[2].skipped)

        val messages = JunitXmlReport.serviceMessages(cases)
        assertEquals(
            "##teamcity[testSuiteStarted name='CalcTest' locationHint='java:suite://com.example.CalcTest']",
            messages.first(),
        )
        assertTrue(messages.any { it.startsWith("##teamcity[testFailed name='fails()' message='expected: <2> but was: <3>' details='stack|ntrace']") })
        assertTrue(messages.any { it == "##teamcity[testIgnored name='later()']" })
        assertEquals("##teamcity[testSuiteFinished name='OtherTest']", messages.last())
    }

    @Test
    fun escapes() {
        assertEquals("|[a|]|'b|'||c|r|n", JunitXmlReport.escape("[a]'b'|c\r\n"))
    }
}
