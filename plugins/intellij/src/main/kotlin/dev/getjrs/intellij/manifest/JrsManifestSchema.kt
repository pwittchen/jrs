package dev.getjrs.intellij.manifest

/**
 * The tables and keys of `jrs.toml`, as `manifest.rs` knows them, and a
 * scanner just good enough to find a manifest's table headers and keys.
 * Tables whose keys are the user's own (`[dependencies]`, `[repositories]`,
 * the `env` tables, …) are never checked.
 */
object JrsManifestSchema {
    val TOP_KEYS = listOf(
        "project", "java", "run", "test", "package", "resources", "obfuscate", "kotlin", "scala", "groovy",
        "dependencies", "dev-dependencies", "managed", "repositories", "tasks", "hooks", "workspace",
    )
    private val PROJECT_KEYS = listOf(
        "name", "version", "main-class", "source-dir", "test-dir", "resource-dir", "test-resource-dir",
        "target-dir", "jrs-version",
    )
    private val JAVA_KEYS = listOf("source", "target", "encoding", "javac-args", "javadoc-args", "jdk")
    private val RUN_KEYS = listOf("jvm-args", "java-agents", "env", "cwd")
    private val TEST_KEYS = listOf(
        "jvm-args", "jacoco-version", "java-agents", "env", "retries", "forks", "coverage-minimum",
        "share-classes", "suites",
    )
    private val SUITE_KEYS = listOf("test-dir", "test-resource-dir", "jvm-args", "env", "retries", "forks")
    private val PACKAGE_KEYS = listOf("add-modules", "manifest", "native-image-args", "relocate")
    private val RESOURCES_KEYS = listOf("expand", "properties")
    private val OBFUSCATE_KEYS = listOf("version", "keep", "proguard-args")
    private val TASK_KEYS = listOf(
        "description", "run", "shell", "script", "main", "dependencies", "args", "depends-on", "env", "cwd",
        "inputs", "outputs", "source-outputs", "resource-outputs", "cache",
    )
    private val WORKSPACE_KEYS = listOf("members")
    val HOOKS = listOf("pre-compile", "post-compile", "pre-test", "post-test", "post-package", "pre-run")

    private fun languageKeys(compiler: String, kotlin: Boolean = false) =
        listOf("version", "source-dir", "test-dir", "$compiler-args", "compiler-jvm-args") +
            if (kotlin) listOf("plugins") else emptyList()

    /** The keys a table may hold, or null when its keys are free-form or not checked. */
    fun keysFor(table: List<String>): List<String>? = when {
        table.isEmpty() -> TOP_KEYS
        table.size == 1 -> when (table[0]) {
            "project" -> PROJECT_KEYS
            "java" -> JAVA_KEYS
            "run" -> RUN_KEYS
            "test" -> TEST_KEYS
            "package" -> PACKAGE_KEYS
            "resources" -> RESOURCES_KEYS
            "obfuscate" -> OBFUSCATE_KEYS
            "kotlin" -> languageKeys("kotlinc", kotlin = true)
            "scala" -> languageKeys("scalac")
            "groovy" -> languageKeys("groovyc")
            "hooks" -> HOOKS
            "workspace" -> WORKSPACE_KEYS
            else -> null
        }
        table.size == 2 && table[0] == "tasks" -> TASK_KEYS
        table.size == 3 && table[0] == "test" && table[1] == "suites" -> SUITE_KEYS
        else -> null
    }

    sealed interface Item {
        val start: Int
        val end: Int

        /** `[a.b]` or `[[a.b]]`; the range covers the name inside the brackets. */
        data class Header(val path: List<String>, override val start: Int, override val end: Int) : Item

        /** A key's dotted segments, relative to its table; the range covers the key. */
        data class Key(val table: List<String>, val segments: List<String>, override val start: Int, override val end: Int) : Item
    }

    data class Problem(val start: Int, val end: Int, val message: String)

    /** The unknown tables and keys, worded as the manifest parser words its warnings. */
    fun problems(text: CharSequence): List<Problem> = scan(text).mapNotNull { item ->
        when (item) {
            is Item.Header -> item.path.firstOrNull()?.takeIf { it !in TOP_KEYS }?.let {
                Problem(item.start, item.end, "unknown table `$it` in jrs.toml (ignored)")
            }
            is Item.Key -> {
                val table = item.table + item.segments.dropLast(1)
                val key = item.segments.lastOrNull() ?: return@mapNotNull null
                val known = keysFor(table) ?: return@mapNotNull null
                if (key in known) {
                    null
                } else {
                    val name = (table + key).joinToString(".")
                    Problem(item.start, item.end, "unknown key `$name` in jrs.toml (ignored)")
                }
            }
        }
    }

    /** What to complete at the end of [linePrefix], the current line up to the caret. */
    data class Completion(val prefix: String, val candidates: List<String>, val table: String)

    private val headerPrefix = Regex("""^\s*\[\[?\s*([A-Za-z0-9_.-]*)$""")
    private val keyPrefix = Regex("""^\s*([A-Za-z0-9_-]*)$""")

    fun completion(before: CharSequence, linePrefix: String): Completion? {
        headerPrefix.matchEntire(linePrefix)?.let { match ->
            val typed = match.groupValues[1]
            val parent = typed.substringBeforeLast('.', "")
            val candidates = if (parent.isEmpty()) TOP_KEYS else keysFor(parent.split('.')).orEmpty()
            return Completion(typed.substringAfterLast('.'), candidates, if (parent.isEmpty()) "table" else "[$parent]")
        }
        keyPrefix.matchEntire(linePrefix)?.let { match ->
            val table = (scan(before).lastOrNull { it is Item.Header } as Item.Header?)?.path.orEmpty()
            val candidates = keysFor(table) ?: return null
            return Completion(match.groupValues[1], candidates, if (table.isEmpty()) "table" else "[${table.joinToString(".")}]")
        }
        return null
    }

    /** The headers and keys of [text], skipping values, strings and comments. */
    fun scan(text: CharSequence): List<Item> {
        val items = ArrayList<Item>()
        var table = emptyList<String>()
        var i = 0
        val n = text.length
        fun skipToEol() {
            while (i < n && text[i] != '\n') i++
        }
        while (i < n) {
            val c = text[i]
            when {
                c == ' ' || c == '\t' || c == '\r' || c == '\n' -> i++
                c == '#' -> skipToEol()
                c == '[' -> {
                    val open = if (i + 1 < n && text[i + 1] == '[') 2 else 1
                    val start = i + open
                    var j = start
                    while (j < n && text[j] != ']' && text[j] != '\n') {
                        if (text[j] == '"' || text[j] == '\'') j = skipQuoted(text, j)
                        j++
                    }
                    val raw = text.subSequence(start, minOf(j, n)).toString()
                    table = splitKey(raw)
                    val lead = raw.length - raw.trimStart().length
                    items += Item.Header(table, start + lead, start + raw.trimEnd().length)
                    i = j
                    skipToEol()
                }
                else -> {
                    val start = i
                    var j = i
                    while (j < n && text[j] != '=' && text[j] != '\n') {
                        if (text[j] == '"' || text[j] == '\'') j = skipQuoted(text, j)
                        j++
                    }
                    val raw = text.subSequence(start, minOf(j, n)).toString()
                    if (j < n && text[j] == '=' && raw.isNotBlank()) {
                        items += Item.Key(table, splitKey(raw), start, start + raw.trimEnd().length)
                        i = skipValue(text, j + 1)
                    } else {
                        i = j
                        skipToEol()
                    }
                }
            }
        }
        return items
    }

    /** From an opening quote at [i], the index of its closing quote (or the end of the line). */
    private fun skipQuoted(text: CharSequence, i: Int): Int {
        val quote = text[i]
        var j = i + 1
        while (j < text.length && text[j] != '\n') {
            if (quote == '"' && text[j] == '\\') {
                j += 2
                continue
            }
            if (text[j] == quote) return j
            j++
        }
        return j - 1
    }

    /** Skips a value, which may span lines inside brackets or a multi-line string. */
    private fun skipValue(text: CharSequence, from: Int): Int {
        var i = from
        var depth = 0
        val n = text.length
        while (i < n) {
            val c = text[i]
            when {
                c == '"' || c == '\'' -> {
                    val triple = "$c$c$c"
                    if (text.startsWith(triple, i)) {
                        val close = text.indexOf(triple, i + 3)
                        i = if (close < 0) n else close + 3
                        continue
                    }
                    i = skipQuoted(text, i) + 1
                    continue
                }
                c == '[' || c == '{' -> depth++
                c == ']' || c == '}' -> depth = maxOf(0, depth - 1)
                c == '#' -> {
                    while (i < n && text[i] != '\n') i++
                    continue
                }
                c == '\n' && depth == 0 -> return i + 1
            }
            i++
        }
        return n
    }

    /** `a."b.c".d` to `[a, b.c, d]`. */
    fun splitKey(raw: String): List<String> {
        val out = ArrayList<String>()
        val current = StringBuilder()
        var quote: Char? = null
        for (c in raw.trim()) {
            when {
                quote != null && c == quote -> quote = null
                quote != null -> current.append(c)
                c == '"' || c == '\'' -> quote = c
                c == '.' -> {
                    out += current.toString().trim()
                    current.setLength(0)
                }
                else -> current.append(c)
            }
        }
        out += current.toString().trim()
        return out.filter { it.isNotEmpty() }
    }
}
