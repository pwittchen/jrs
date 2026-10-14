package dev.getjrs.intellij

import dev.getjrs.intellij.run.JrsOutput
import org.junit.Assert.assertEquals
import org.junit.Test

/** jrs's stderr, as `--progress never --color never` writes it: only errors are errors. */
class JrsOutputTest {
    /** The lines of [transcript] that [JrsOutput] calls errors. */
    private fun errors(transcript: String): List<String> {
        val output = JrsOutput()
        return transcript.trimIndent().lines().filter(output::isError)
    }

    @Test
    fun `a build, a test run and a run are no errors`() {
        val transcript = """
            |   Resolving 1 declared dependencies
            | Downloading 11 artifacts
            |    Restored core v1.0.0 (from the build cache)
            |   Compiling journal v1.0.0 (1 source files)
            |       Fresh app v1.0.0
            |    Finished build in 67ms
            |       build ok      9 classes in 3 modules
            |        deps 11      0 downloaded
            |        time 67ms
            |    Restored core: 1 test sources (from the build cache)
            |     Testing core: 1 test class: passed in the cached run (`--all` runs them)
            |   Reporting test results into /work/core/target/test-reports/index.html
            |    Finished 3 tests, 3 passed in 1 module in 74ms
            |  Looking up the newest release of g:a
            |     Running com.example.ledger.app.Main
            |
            |+ javac @/work/target/.jrs/javac-main.args
        """.trimMargin()
        assertEquals(emptyList<String>(), errors(transcript))
    }

    @Test
    fun `a warning and its hint are no errors`() {
        val transcript = """
            |warning: `g:a` is declared at 1.0 by `core` and at 2.0 by `app`; the workspace pins one version
            |
            |  set it once in the root's [managed]
            |   Compiling app v1.0.0 (2 source files)
            |Note: Some input files use unchecked or unsafe operations.
            |1 warning
            |/work/App.java:3: warning: [deprecation] old() in Old has been deprecated
            |        old();
            |        ^
        """.trimMargin()
        assertEquals(emptyList<String>(), errors(transcript))
    }

    @Test
    fun `a failed compile is an error, from the diagnostic and from jrs`() {
        val transcript = """
            |   Compiling core v1.0.0 (4 source files)
            |/work/core/src/main/java/Money.java:47: error: cannot find symbol
            |        return byCurrencyX != 0 ? byCurrency : 0;
            |               ^
            |  symbol:   variable byCurrencyX
            |  location: class Money
            |1 error
            |error: compilation failed (1 source files)
            |
            |check the source above
        """.trimMargin()
        assertEquals(
            listOf(
                "/work/core/src/main/java/Money.java:47: error: cannot find symbol",
                "        return byCurrencyX != 0 ? byCurrency : 0;",
                "               ^",
                "  symbol:   variable byCurrencyX",
                "  location: class Money",
                "1 error",
                "error: compilation failed (1 source files)",
                "",
                "check the source above",
            ),
            errors(transcript),
        )
    }

    @Test
    fun `a program's own stderr stays as it was written`() {
        val transcript = """
            |     Running com.example.Main
            |Exception in thread "main" java.lang.IllegalStateException: boom
            |	at com.example.Main.main(Main.java:5)
            |    Finished com.example.Main exited with 1
        """.trimMargin()
        assertEquals(
            listOf(
                "Exception in thread \"main\" java.lang.IllegalStateException: boom",
                "\tat com.example.Main.main(Main.java:5)",
            ),
            errors(transcript),
        )
    }
}
