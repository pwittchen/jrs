package dev.getjrs.intellij.run

import org.w3c.dom.Element
import java.nio.file.Files
import java.nio.file.Path
import javax.xml.XMLConstants
import javax.xml.parsers.DocumentBuilderFactory
import kotlin.io.path.getLastModifiedTime
import kotlin.io.path.isRegularFile
import kotlin.io.path.listDirectoryEntries
import kotlin.io.path.name

/** The `TEST-*.xml` reports `jrs test` writes into `target/test-reports` (SPEC §10.2). */
object JunitXmlReport {
    const val SUITE_PROTOCOL = "java:suite"
    const val TEST_PROTOCOL = "java:test"

    data class TestCase(
        val className: String,
        val name: String,
        val durationMillis: Long,
        val failure: String?,
        val failureMessage: String?,
        val skipped: Boolean,
        val output: String?,
    ) {
        /** The method name without JUnit's `()` or display-name parameters. */
        val methodName: String get() = name.substringBefore('(').trim()
    }

    /** The test cases of every report written at or after [since] (a run's own reports). */
    fun read(dir: Path, since: Long): List<TestCase> {
        if (!Files.isDirectory(dir)) return emptyList()
        return dir.listDirectoryEntries("TEST-*.xml")
            .filter { it.isRegularFile() && it.getLastModifiedTime().toMillis() >= since - 2_000 }
            .sortedBy { it.name }
            .flatMap { file -> runCatching { parse(Files.readAllBytes(file)) }.getOrDefault(emptyList()) }
    }

    fun parse(xml: ByteArray): List<TestCase> {
        val factory = DocumentBuilderFactory.newInstance().apply {
            setFeature(XMLConstants.FEATURE_SECURE_PROCESSING, true)
            setFeature("http://apache.org/xml/features/disallow-doctype-decl", true)
        }
        val document = factory.newDocumentBuilder().parse(xml.inputStream())
        val cases = document.getElementsByTagName("testcase")
        return (0 until cases.length).map { i ->
            val case = cases.item(i) as Element
            val failure = case.child("failure") ?: case.child("error")
            TestCase(
                className = case.getAttribute("classname"),
                name = case.getAttribute("name"),
                durationMillis = ((case.getAttribute("time").toDoubleOrNull() ?: 0.0) * 1000).toLong(),
                failure = failure?.textContent,
                failureMessage = failure?.getAttribute("message"),
                skipped = case.child("skipped") != null,
                output = case.child("system-out")?.textContent?.takeIf { it.isNotBlank() },
            )
        }
    }

    /** TeamCity service messages that build the IDE's test tree, one suite per class. */
    fun serviceMessages(cases: List<TestCase>): List<String> = buildList {
        for ((className, tests) in cases.groupBy { it.className }) {
            val suite = className.substringAfterLast('.')
            add(message("testSuiteStarted", "name" to suite, "locationHint" to "$SUITE_PROTOCOL://$className"))
            for (test in tests) {
                add(message("testStarted", "name" to test.name, "locationHint" to "$TEST_PROTOCOL://$className/${test.methodName}"))
                test.output?.let { add(message("testStdOut", "name" to test.name, "out" to it)) }
                when {
                    test.skipped -> add(message("testIgnored", "name" to test.name))
                    test.failure != null -> add(
                        message(
                            "testFailed",
                            "name" to test.name,
                            "message" to (test.failureMessage ?: ""),
                            "details" to test.failure,
                        ),
                    )
                }
                add(message("testFinished", "name" to test.name, "duration" to test.durationMillis.toString()))
            }
            add(message("testSuiteFinished", "name" to suite))
        }
    }

    private fun message(type: String, vararg attributes: Pair<String, String>): String =
        attributes.joinToString(" ", prefix = "##teamcity[$type ", postfix = "]") { (k, v) -> "$k='${escape(v)}'" }

    /** TeamCity's escaping: `|` before `'`, `[`, `]`, `|` and the line breaks. */
    fun escape(value: String): String = buildString {
        for (c in value) {
            when (c) {
                '|' -> append("||")
                '\'' -> append("|'")
                '[' -> append("|[")
                ']' -> append("|]")
                '\n' -> append("|n")
                '\r' -> append("|r")
                '\u0085' -> append("|x")
                '\u2028' -> append("|l")
                '\u2029' -> append("|p")
                else -> append(c)
            }
        }
    }

    private fun Element.child(tag: String): Element? {
        val nodes = getElementsByTagName(tag)
        return if (nodes.length > 0) nodes.item(0) as Element else null
    }
}
