package dev.getjrs.intellij.build

/**
 * Recognises a compiler diagnostic in jrs's plain transcript, which passes
 * `javac`'s and `kotlinc`'s output through verbatim.
 */
object CompilerDiagnostics {
    data class Diagnostic(val path: String, val line: Int, val column: Int?, val error: Boolean, val message: String)

    // javac: /work/app/src/main/java/App.java:12: error: cannot find symbol
    private val javac = Regex("""^(.+\.(?:java|groovy|scala)):(\d+):(?:(\d+):)?\s*(error|warning):\s*(.*)$""")

    // kotlinc: e: file:///work/app/src/main/kotlin/App.kt:12:5 Unresolved reference 'x'.
    private val kotlinc = Regex("""^([ew]): (?:file://)?(.+\.kts?):(\d+):(\d+):? (.*)$""")

    // scalac: [error] /work/app/src/main/scala/App.scala:12:5: not found: value x
    private val scalac = Regex("""^\[(error|warn)]\s+(.+\.scala):(\d+):(\d+):\s*(.*)$""")

    fun parse(line: String): Diagnostic? {
        javac.matchEntire(line)?.destructured?.let { (path, row, col, kind, message) ->
            return Diagnostic(path, row.toInt(), col.toIntOrNull(), kind == "error", message)
        }
        kotlinc.matchEntire(line)?.destructured?.let { (kind, path, row, col, message) ->
            return Diagnostic(path, row.toInt(), col.toIntOrNull(), kind == "e", message)
        }
        scalac.matchEntire(line)?.destructured?.let { (kind, path, row, col, message) ->
            return Diagnostic(path, row.toInt(), col.toIntOrNull(), kind == "error", message)
        }
        return null
    }
}
