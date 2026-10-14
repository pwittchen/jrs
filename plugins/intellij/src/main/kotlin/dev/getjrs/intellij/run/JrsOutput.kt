package dev.getjrs.intellij.run

import dev.getjrs.intellij.build.CompilerDiagnostics

/**
 * Sorts the lines jrs writes to stderr into errors and the rest. jrs puts its
 * whole transcript on stderr, as Cargo does — phase lines, the summary,
 * warnings — and a console paints stderr red, so without this every build
 * would look like a failure. One instance per process: it remembers where
 * jrs's closing `error:` began and whether a compiler diagnostic's source
 * lines are still coming.
 */
class JrsOutput {
    /** Set once jrs has printed `error:`: the message's other lines follow it. */
    private var inError = false

    /** Set after `warning:`, while its blank and indented lines follow. */
    private var inWarning = false

    /**
     * Lines still to come of a compiler diagnostic — its source line and
     * caret, and then any indented line (`symbol:`, `location:`) — and their kind.
     */
    private var following = 0
    private var inDiagnostic = false
    private var followingError = false

    /** Whether a stderr [line] is an error, which a console shows as such. */
    fun isError(line: String): Boolean {
        if (inError) return true
        if (line.isBlank()) return false
        if (line.startsWith("error:") || line.startsWith("error[")) {
            inError = true
            return true
        }
        CompilerDiagnostics.parse(line)?.let {
            following = 2
            inDiagnostic = true
            followingError = it.error
            return it.error
        }
        if (following > 0 || (inDiagnostic && line.startsWith(" "))) {
            if (following > 0) following--
            return followingError
        }
        inDiagnostic = false
        if (line.startsWith("warning:")) {
            inWarning = true
            return false
        }
        if (inWarning && (line.isBlank() || line.startsWith("  ")) && !isJrsLine(line)) return false
        inWarning = false
        if (isJrsLine(line)) return false
        // A javac note, or its count of warnings, is not an error either.
        if (line.startsWith("Note: ") || WARNING_COUNT.matches(line.trim())) return false
        // Anything else is someone else's stderr — the program `jrs run`
        // started, a task, a test — and stays as it was written.
        return true
    }

    companion object {
        private val WARNING_COUNT = Regex("""\d+ warnings?""")

        /**
         * One of jrs's own lines: `warning: …`, a `-v` line (`+ …`), or a
         * phase line or summary row, whose word is right-aligned in the first
         * twelve columns (`   Compiling app v1.0.0`, `       build ok …`).
         */
        fun isJrsLine(line: String): Boolean {
            if (line.startsWith("warning:") || line.startsWith("+ ")) return true
            if (line.length <= 12 || line[12] != ' ') return false
            val head = line.substring(0, 12)
            return head.first() == ' ' && head.last() != ' ' && head.trimStart().all { it.isLetter() || it == ' ' }
        }
    }
}
